// The seed feed is what these tests look at: they run in browser mode (support/app.ts), so the
// library that tests/library.spec.ts fills at the same time never shows up here.
import { expect, test } from './support/app';

const backendUrl = 'http://127.0.0.1:8001';

test('serves the Instagram clone frontend through the live FastAPI backend', async ({ page }) => {
  const healthResponse = await page.request.get('/api/health');

  expect(healthResponse.ok()).toBeTruthy();
  await expect(healthResponse.json()).resolves.toEqual({ status: 'ok' });

  await page.goto('/');
  await expect(page).toHaveTitle('Instagram');
  await expect(page.getByRole('navigation', { name: 'Main navigation' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Home' })).toBeVisible();
  await expect(page.getByText('pocketdiarybookclub').first()).toBeVisible();
});

test('likes a post and switches appearance', async ({ page, request }) => {
  const counterResponse = await request.get(`${backendUrl}/api/counter`);
  expect(counterResponse.ok()).toBeTruthy();

  await page.goto('/');
  await expect(page.getByRole('button', { name: 'Like', exact: true }).first()).toBeVisible();
  await page.getByRole('button', { name: 'Like', exact: true }).first().click();
  await expect(page.getByRole('button', { name: 'Unlike', exact: true }).first()).toBeVisible();

  await page.getByRole('button', { name: 'More', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Switch appearance' }).click();
  await expect(page.locator('html')).toHaveClass(/__fb-dark-mode/);
});
