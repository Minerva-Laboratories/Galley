import { expect, test } from '@playwright/test';
import { authenticate, dismissTour } from './auth';

test('accent colors preview live, persist across pages, and keep errors distinct', async ({ page, context }, testInfo) => {
  await authenticate(page);
  await page.getByRole('button', { name: 'Appearance', exact: true }).click();
  const appearance = page.getByRole('dialog', { name: 'Appearance', exact: true });
  await expect(appearance).toBeVisible();
  await appearance.getByRole('button', { name: 'Ocean accent' }).click();
  await expect(appearance.getByLabel('Accent hex color')).toHaveValue('#287BCC');
  const primary = appearance.getByRole('button', { name: 'Done', exact: true });
  await expect(primary).toHaveCSS('background-color', 'rgb(40, 123, 204)');
  await appearance.getByRole('button', { name: 'Dark', exact: true }).click();
  await page.screenshot({ path: testInfo.outputPath('appearance-dark.png') });
  await appearance.getByLabel('Custom accent color').fill('#ffff00');
  await expect(primary).toHaveCSS('color', 'rgb(0, 0, 0)');
  await appearance.getByLabel('Accent hex color').fill('badhex');
  await appearance.getByLabel('Accent hex color').press('Enter');
  await expect(appearance.getByRole('alert')).toBeVisible();
  await expect(primary).toHaveCSS('background-color', 'rgb(255, 255, 0)');
  await appearance.getByLabel('Accent hex color').fill('#7c3aed');
  await appearance.getByLabel('Accent hex color').press('Enter');
  await expect(primary).toHaveCSS('background-color', 'rgb(124, 58, 237)');
  await page.keyboard.press('Escape');
  await expect(appearance).toBeHidden();
  await expect(page.getByRole('button', { name: 'Appearance', exact: true })).toBeFocused();
  await page.reload();
  await page.getByRole('button', { name: 'Appearance', exact: true }).click();
  await expect(appearance.getByLabel('Accent hex color')).toHaveValue('#7C3AED');

  const other = await context.newPage();
  await other.goto('/');
  await other.getByRole('button', { name: 'Appearance', exact: true }).click();
  await appearance.getByRole('button', { name: 'Teal accent' }).click();
  await expect(other.getByLabel('Accent hex color')).toHaveValue('#168B86');
  await appearance.getByRole('button', { name: 'Light', exact: true }).click();
  await expect(other.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.screenshot({ path: testInfo.outputPath('appearance-light.png') });
  await page.setViewportSize({ width: 375, height: 700 });
  const box = (await appearance.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(375);
  await page.screenshot({ path: testInfo.outputPath('appearance-mobile.png') });
  await primary.click();
  await page.setViewportSize({ width: 1280, height: 800 });

  const csrf = (await context.cookies()).find((cookie) => cookie.name === 'galley_csrf')!.value;
  const created = await page.request.post('/api/projects', { headers: { 'x-csrf-token': csrf }, data: { name: `Appearance ${Date.now()}` } });
  expect(created.status()).toBe(201);
  const { id } = await created.json();
  await page.route(`**/api/projects/${id}/build`, (route) => route.fulfill({ json: {
    running: false, last: { id: 1, status: 'failed', pdf_available: true, pdf_fresh: false, engine: 'tectonic',
      errors: [], error_count: 1, warning_count: 0, finished_at: '2026-01-01T00:00:00Z', profile: { total_ms: 1 } },
  } }));
  await page.goto(`/p/${id}`);
  await dismissTour(page);
  await expect(page.getByRole('button', { name: /^Build/ }).filter({ hasText: 'Build' }).last()).toHaveCSS('background-color', 'rgb(22, 139, 134)');
  await expect(page.locator('.stale')).toHaveCSS('background-color', 'rgb(198, 64, 42)');
  await page.getByRole('button', { name: 'Appearance', exact: true }).click();
  await expect(appearance.getByLabel('Accent hex color')).toHaveValue('#168B86');
  await appearance.getByRole('button', { name: 'Reset accent' }).click();
  await expect(appearance.getByLabel('Accent hex color')).toHaveValue('#C6402A');
  await expect(other.getByLabel('Accent hex color')).toHaveValue('#C6402A');
  await primary.click();
  await other.close();
});
