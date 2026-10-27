import { expect, test } from '@playwright/test';
import { authenticate, dismissTour } from './auth';

// A tab left alone lets go of its connections, so the server can stop, and comes back on the
// first key press with nothing lost.
test('an idle tab disconnects, then reconnects and keeps every edit', async ({ page }) => {
  await page.clock.install();
  await authenticate(page);
  const csrf = (await page.context().cookies()).find((cookie) => cookie.name === 'galley_csrf')!.value;
  const created = await page.request.post('/api/projects', { headers: { 'x-csrf-token': csrf }, data: { name: `Away ${Date.now()}` } });
  expect(created.ok()).toBeTruthy();
  const { id } = await created.json();

  const open = new Set<object>();
  page.on('websocket', (ws) => {
    open.add(ws);
    ws.on('close', () => open.delete(ws));
  });
  await page.goto(`/p/${id}`);
  await dismissTour(page);
  await page.locator('.cm-content').click();
  await page.keyboard.press('Control+End');
  await page.keyboard.insertText('\nWritten before the break.');
  await expect.poll(() => open.size).toBeGreaterThanOrEqual(2);

  await page.clock.fastForward('31:00');
  await expect(page.locator('.pill', { hasText: 'Paused while you were away' })).toBeVisible();
  // Through a proxy the closing handshake can take several seconds.
  await expect.poll(() => open.size, { message: 'every socket is closed while away', timeout: 30_000 }).toBe(0);

  await page.locator('.cm-content').click();
  await page.keyboard.press('Control+End');
  await page.keyboard.insertText('\nWritten after the break.');
  await expect(page.locator('.pill', { hasText: 'Paused while you were away' })).toBeHidden();
  await expect.poll(() => open.size).toBeGreaterThanOrEqual(2);

  await expect.poll(async () => {
    const res = await page.request.get(`/api/projects/${id}/files/content?path=main.tex`);
    return res.ok() ? await res.text() : '';
  }, { timeout: 15_000 }).toContain('Written after the break.');
  const saved = await (await page.request.get(`/api/projects/${id}/files/content?path=main.tex`)).text();
  expect(saved).toContain('Written before the break.');
});
