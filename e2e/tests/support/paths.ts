/** Paths shared by playwright.config.ts and the specs (no Playwright import: the config loads it). */
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const E2E_ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

/**
 * The library folder of the backend the suite starts (INSTAGRAM_CACHE_DIR). In real use it is
 * %TEMP%\instagram_cache; the tests never touch that one.
 */
export const CACHE_DIR = join(E2E_ROOT, 'test-results', 'instagram_cache');
