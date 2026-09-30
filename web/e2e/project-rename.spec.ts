import { expect, test, type Page } from '@playwright/test';
import type { ProjectView, Role2 } from '../src/api';

// Exercise the UI with a deterministic API; the Rust integration test covers persistence and auth.
async function mockProject(page: Page, role: Role2 = 'admin') {
  let project: ProjectView = {
    id: 'original', name: 'Original', role, main_file: 'main.tex',
    created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-01T00:00:00Z',
  };
  const writes: string[] = [];
  let fail = false;
  await page.addInitScript(() => localStorage.setItem('galley.tour', 'done'));
  await page.route('**/api/**', async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === '/api/projects/original' && request.method() === 'PATCH') {
      writes.push(request.postDataJSON().name as string);
      if (fail) {
        await route.fulfill({ status: 500, json: { error: 'Could not save the name.' } });
        return;
      }
      project = { ...project, name: request.postDataJSON().name as string, updated_at: '2026-09-30T12:00:00Z' };
      await route.fulfill({ json: project });
      return;
    }
    const body = path === '/api/auth/me'
      ? { user: { id: 'me', name: 'Maintainer', email: 'me@example.com', is_admin: true, is_guest: false }, needs_setup: false, public_signup: false }
      : path === '/api/projects' ? [project]
      : path === '/api/projects/original' ? project
      : path.endsWith('/build') ? null : [];
    await route.fulfill({ json: body });
  });
  return { writes, failNext: (value: boolean) => { fail = value; } };
}

test('rename from Projects updates the card and persists across reload without navigating', async ({ page }) => {
  const { writes } = await mockProject(page);
  await page.goto('/');
  await page.getByRole('button', { name: 'Rename Original', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Rename project' });
  await expect(dialog.getByLabel('Project name')).toHaveValue('Original');
  await expect(dialog.getByLabel('Project name')).toBeFocused();
  await dialog.getByLabel('Project name').fill('  Nueva investigación 📝  ');
  await dialog.getByLabel('Project name').press('Enter');
  await expect(dialog).not.toBeVisible();
  await expect(page.locator('.pcard .t')).toHaveText('Nueva investigación 📝');
  await expect(page).toHaveURL(/\/$/);
  expect(writes).toEqual(['Nueva investigación 📝']);
  await page.reload();
  await expect(page.locator('.pcard .t')).toHaveText('Nueva investigación 📝');
  await page.locator('.pcard-open').click();
  await expect(page).toHaveURL(/\/p\/original$/);
});

test('cancel, name validation and failed saves preserve the existing name and allow retry', async ({ page }) => {
  const mock = await mockProject(page);
  await page.goto('/');
  const trigger = page.getByRole('button', { name: 'Rename Original', exact: true });
  await trigger.click();
  const dialog = page.getByRole('dialog', { name: 'Rename project' });
  const input = dialog.getByLabel('Project name');
  await input.fill('Cancelled');
  await input.press('Escape');
  await expect(dialog).not.toBeVisible();
  await expect(trigger).toBeFocused();
  expect(mock.writes).toEqual([]);

  await trigger.click();
  await input.fill('   ');
  await expect(dialog.getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
  await input.fill('界'.repeat(121));
  await expect(input).toHaveAttribute('aria-invalid', 'true');
  await expect(dialog.getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
  await input.fill('Renamed');
  mock.failNext(true);
  await dialog.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(dialog.getByRole('alert')).toContainText('Could not save the name.');
  await expect(input).toHaveValue('Renamed');
  await expect(page.locator('.pcard .t')).toHaveText('Original');
  mock.failNext(false);
  await dialog.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('.pcard .t')).toHaveText('Renamed');
});

test('rename in the editor and collaborator events update the header and browser title with the same URL', async ({ page }) => {
  await mockProject(page);
  let sendEvent: ((message: string) => void) | undefined;
  await page.routeWebSocket('**/ws/original/events', (socket) => { sendEvent = (message) => socket.send(message); });
  await page.goto('/p/original');
  await expect(page.locator('.proj')).toContainText('Original');
  await page.getByRole('button', { name: 'Rename project', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Rename project' });
  await dialog.getByLabel('Project name').fill('From editor');
  await dialog.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('.proj')).toContainText('From editor');
  await expect(page).toHaveTitle('From editor — Galley');
  await expect(page).toHaveURL(/\/p\/original$/);
  await expect.poll(() => !!sendEvent).toBe(true);
  sendEvent!(JSON.stringify({ type: 'project_renamed', name: 'From collaborator', updated_at: '2026-09-30T13:00:00Z' }));
  await expect(page.locator('.proj')).toContainText('From collaborator');
  await expect(page).toHaveTitle('From collaborator — Galley');
});

for (const role of ['editor', 'commenter', 'viewer'] as const) {
  test(`${role} cannot rename from Projects or the editor`, async ({ page }) => {
    await mockProject(page, role);
    await page.routeWebSocket('**/ws/**', () => {});
    await page.goto('/');
    await expect(page.locator('.pcard .t')).toHaveText('Original');
    await expect(page.getByRole('button', { name: 'Rename Original', exact: true })).toHaveCount(0);
    await page.locator('.pcard-open').click();
    await expect(page.locator('.proj')).toContainText('Original');
    await expect(page.getByRole('button', { name: 'Rename project', exact: true })).toHaveCount(0);
  });
}
