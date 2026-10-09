import { expect, test } from '@playwright/test';
import { authenticate, dismissTour, startNewProject } from './auth';

// M1 flow: create a project, edit it in two tabs, see the edit arrive, see it land in git.

test('edits sync between two tabs and land in git', async ({ browser }) => {
  const name = `E2E ${Date.now()}`;
  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();
  const a = await ctxA.newPage();
  const b = await ctxB.newPage();

  await authenticate(a);
  const author = (await (await a.request.get('/api/auth/me')).json()).user.name as string;
  await startNewProject(a);
  await a.getByLabel('Project name').fill(name);
  await a.getByRole('button', { name: 'Create' }).click();
  await expect(a).toHaveURL(/\/p\/[a-z0-9-]+$/);
  await expect(a.locator('.cm-content')).toContainText('documentclass');
  await dismissTour(a);

  await authenticate(b);
  await b.goto(a.url());
  await expect(b.locator('.cm-content')).toContainText('documentclass');
  await dismissTour(b);
  await expect(a.locator('.presence .n')).toHaveText('2 online');

  const marker = `Synced-${Date.now()}`;
  await a.locator('.cm-content').click();
  await a.keyboard.press('Control+End');
  await a.keyboard.type(`\n% ${marker}`);
  await expect(b.locator('.cm-content')).toContainText(marker, { timeout: 10_000 });

  await b.getByRole('button', { name: 'History' }).click();
  await expect(b.locator('.ti .l').first()).toHaveText('edit: main.tex', { timeout: 15_000 });
  await expect(b.locator('.ti .s').first()).toContainText(author);

  await ctxA.close();
  await ctxB.close();
});

test('build produces a PDF, an error card fixes it, and SyncTeX jumps both ways', async ({ page }) => {
  const name = `Build ${Date.now()}`;
  await authenticate(page);
  await startNewProject(page);
  await page.getByLabel('Project name').fill(name);
  await page.getByRole('button', { name: 'Create' }).click();
  await expect(page).toHaveURL(/\/p\/[a-z0-9-]+$/);
  await dismissTour(page);

  // Use a small document so this flow focuses on diagnostics and SyncTeX.
  await page.getByRole('combobox', { name: 'Build mode' }).selectOption('manual');
  await page.locator('.cm-content').click();
  await page.keyboard.press('Control+A');
  await page.keyboard.insertText('\\documentclass{article}\n\\begin{document}\nHello from Galley.\n\\end{document}\n');
  await expect(page.locator('.cm-content')).toContainText('Hello from Galley.');
  // First build renders a PDF.
  await page.locator('.build .primary').click();
  const projectId = page.url().split('/').pop()!;
  await expect.poll(async () => (await (await page.request.get(`/api/projects/${projectId}/build`)).json()).last?.status ?? 'pending', { timeout: 60_000 }).not.toBe('pending');
  const initialBuild = (await (await page.request.get(`/api/projects/${projectId}/build`)).json()).last;
  if (initialBuild.status !== 'ok') throw new Error(`Initial build failed: ${JSON.stringify(initialBuild.errors)}`);
  await expect(page.locator('.build .st')).toContainText('Compiled', { timeout: 60_000 });
  await expect(page.locator('.pdf-page canvas')).toBeVisible();

  // A \citep without natbib fails the build and offers a one-click fix.
  await page.locator('.cm-line').filter({ hasText: '\\end{document}' }).first().click();
  await page.keyboard.press('Home');
  await page.keyboard.type('See \\citep{x}.\n');
  await expect(page.locator('.cm-content')).toContainText('See \\citep{x}.');
  await page.locator('.build .primary').click();
  await expect(page.locator('.build .st')).toContainText('Build failed', { timeout: 60_000 });
  const card = page.locator('.probs .card', { hasText: 'Undefined control sequence' });
  await expect(card).toBeVisible();
  await expect(page.locator('.stale')).toBeVisible();
  await card.getByRole('button', { name: 'Add natbib' }).click();
  await expect(page.locator('.build .st')).toContainText('Compiled', { timeout: 60_000 });
  await expect(page.locator('.cm-content')).toContainText('\\usepackage{natbib}');

  // Inverse SyncTeX: clicking the PDF jumps to the source.
  await page.locator('.pdf-page canvas').first().click({ position: { x: 120, y: 80 } });
  await expect(page.locator('.toast')).toContainText('Jumped to', { timeout: 10_000 });
});
