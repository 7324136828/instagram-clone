// The callbacks passed to page.evaluate() run in the browser, so this file needs the DOM types
// (the e2e tsconfig only has the Node ones).
/// <reference lib="dom" />
/// <reference lib="dom.iterable" />
/**
 * What the import specs share: the `test` with its network rules, the storage mode, fixture
 * paths and the locators of the import UI.
 */
import { expect, test as base, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { basename, extname, join } from 'node:path';
import { CACHE_DIR, E2E_ROOT } from './paths';

export { CACHE_DIR };

const fixtures = join(E2E_ROOT, 'fixtures');
export const fixture = (name: string) => join(fixtures, name);

export function isInstagramUrl(value: string): boolean {
  const { protocol, hostname } = new URL(value);
  return protocol === 'https:' && (hostname === 'instagram.com' || hostname.endsWith('.instagram.com'));
}

/**
 * - Nothing may leave the machine. Requests to another origin are recorded; instagram.com is
 *   answered with an empty stub page (so "View on Instagram" can be followed), the rest is aborted.
 * - `libraryMode` (an option, default false): without it GET /api/library answers 503, so the app
 *   runs in browser mode (IndexedDB) exactly as it does when the library cannot be used. With it,
 *   imports are saved in the backend's library (CACHE_DIR); only tests/library.spec.ts uses it,
 *   because those tests share that one folder and run one after the other.
 */
export const test = base.extend<{ outbound: string[]; libraryMode: boolean; storage: void }>({
  outbound: [
    async ({ context, baseURL }, use) => {
      const appOrigin = new URL(baseURL ?? 'http://127.0.0.1:5174').origin;
      const seen: string[] = [];
      await context.route(
        (url) => (url.protocol === 'http:' || url.protocol === 'https:') && url.origin !== appOrigin,
        async (route) => {
          const url = route.request().url();
          seen.push(url);
          if (isInstagramUrl(url)) {
            await route.fulfill({
              status: 200,
              contentType: 'text/html',
              body: '<!doctype html><title>Stubbed Instagram</title><p>stub</p>',
            });
          } else {
            await route.abort();
          }
        },
      );
      await use(seen);
    },
    { auto: true },
  ],
  libraryMode: [false, { option: true }],
  storage: [
    async ({ context, libraryMode }, use) => {
      if (!libraryMode) {
        await context.route((url) => url.pathname === '/api/library', (route) => route.fulfill({
          status: 503,
          contentType: 'application/json',
          body: JSON.stringify({ detail: 'The library is switched off for this test' }),
        }));
      }
      await use();
    },
    { auto: true },
  ],
});

export { expect };

export const SEED_USER = 'pocketdiarybookclub';

export const importDialog = (page: Page) => page.getByRole('dialog', { name: 'Import from Instagram export' });
export const importStatus = (page: Page) => page.getByTestId('import-status');
export const banner = (page: Page) => page.getByTestId('import-banner');
export const card = (page: Page, id: string) => page.locator(`article.post[data-post-id="imp:${id}"]`);

export async function openImportDialog(page: Page): Promise<void> {
  await page.getByTestId('import-open').click();
  await expect(importDialog(page)).toBeVisible();
}

export async function expectImportSuccess(page: Page, statusText: string | RegExp): Promise<void> {
  const status = importStatus(page);
  // HEIC conversion downloads and starts a WebAssembly decoder: allow for a cold dev server.
  await expect(status).toHaveAttribute('data-state', 'success', { timeout: 60_000 });
  await expect(status).toHaveText(statusText);
}

export async function viewFeed(page: Page): Promise<void> {
  await page.getByRole('button', { name: 'View feed' }).click();
  await expect(importDialog(page)).toHaveCount(0);
  await expect(banner(page)).toBeVisible();
}

/** The MIME type a browser reports for a dragged file (it goes by the extension). */
const MIME_BY_EXTENSION: Record<string, string> = {
  '.zip': 'application/zip',
  '.gz': 'application/gzip',
  '.json': 'application/json',
  '.jpg': 'image/jpeg',
  '.png': 'image/png',
  '.webp': 'image/webp',
  '.heic': 'image/heic',
};

/**
 * A DataTransfer that carries the given fixture files, built inside the page (it cannot cross
 * the bridge). Like files dragged out of a file manager, each File only has its base name.
 */
export async function dataTransferWithFiles(page: Page, files: string[]) {
  const payload = files.map((file) => ({
    name: basename(file),
    base64: readFileSync(fixture(file)).toString('base64'),
    type: MIME_BY_EXTENSION[extname(file).toLowerCase()] ?? '',
  }));
  return page.evaluateHandle((items) => {
    const transfer = new DataTransfer();
    for (const item of items) {
      const binary = atob(item.base64);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
      transfer.items.add(new File([bytes], item.name, { type: item.type }));
    }
    return transfer;
  }, payload);
}

/** How far the document can be scrolled sideways (0 when it fits the viewport). */
export const sidewaysOverflow = (page: Page) =>
  page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
