import { expect, test } from '@playwright/test';
import { authenticate, dismissTour } from './auth';

test('Codex setup supports both terminals and app configuration without recreating the token', async ({ page, context }, testInfo) => {
  await authenticate(page);
  const csrf = (await context.cookies()).find((cookie) => cookie.name === 'galley_csrf')!.value;
  const created = await page.request.post('/api/projects', { headers: { 'x-csrf-token': csrf }, data: { name: `Connect AI ${Date.now()}` } });
  expect(created.status()).toBe(201);
  const { id } = await created.json();
  let creations = 0;
  await page.route('**/api/auth/tokens', (route) => {
    if (route.request().method() === 'POST') {
      creations++;
      return route.fulfill({ json: { token: 'galley-example-token', id: 'example', label: route.request().postDataJSON().label } });
    }
    return route.fulfill({ json: [] });
  });
  await page.goto(`/p/${id}`);
  await dismissTour(page);
  await page.getByRole('button', { name: 'Connect AI', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Connect an AI client' });
  await expect(dialog.getByLabel('AI client', { exact: true })).toHaveValue('codex');
  await expect(dialog.getByLabel('Client name')).toHaveValue('OpenAI Codex');
  await dialog.getByLabel('AI client', { exact: true }).selectOption('claude');
  await expect(dialog.getByLabel('Client name')).toHaveValue('Claude Code');
  await dialog.getByLabel('Client name').fill('My assistant');
  await dialog.getByLabel('AI client', { exact: true }).selectOption('codex');
  await expect(dialog.getByLabel('Client name')).toHaveValue('My assistant');
  await dialog.getByRole('button', { name: 'Create token', exact: true }).click();
  const commands = dialog.locator('.mcp-command pre').first();
  await expect(commands).toContainText('export GALLEY_');
  await expect(commands).toContainText(`codex mcp add 'galley-${id}'`);
  await expect(commands).toContainText(`/mcp/${id}`);
  await expect(commands).toContainText('--bearer-token-env-var');
  await expect(commands).toContainText('galley-example-token');
  await dialog.getByLabel('Terminal', { exact: true }).selectOption('powershell');
  await expect(commands).toContainText('$env:GALLEY_');
  await expect(commands).not.toContainText('export ');
  await dialog.getByText('Using the Codex app or IDE extension?', { exact: true }).click();
  const config = dialog.locator('.codex-config pre');
  await expect(config).toContainText(`[mcp_servers."galley-${id}"]`);
  await expect(config).toContainText('http_headers = { Authorization = "Bearer galley-example-token" }');
  await expect(dialog.getByRole('button', { name: 'Copy configuration' })).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath('codex-connect.png') });
  await page.setViewportSize({ width: 375, height: 700 });
  const box = (await dialog.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(375);
  expect(await dialog.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath('codex-connect-mobile.png') });
  await dialog.getByLabel('AI client', { exact: true }).selectOption('claude');
  await expect(commands).toContainText('claude mcp add --transport http');
  await dialog.getByLabel('AI client', { exact: true }).selectOption('other');
  await expect(dialog.getByText('Configure your MCP client.')).toBeVisible();
  expect(creations).toBe(1);
  await dialog.getByRole('button', { name: 'Done', exact: true }).click();
  await expect(dialog).toBeHidden();
});
