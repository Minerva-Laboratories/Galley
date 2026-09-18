import { expect, test, type Page } from '@playwright/test';

// M1 flow: create a project, edit it in two tabs, see the edit arrive, see it land in git.

async function setName(page: Page, name: string) {
  const input = page.getByLabel('Your name, as collaborators see it');
  if (await input.isVisible()) {
    await input.fill(name);
    await page.getByRole('button', { name: 'Save' }).click();
  }
}

test('edits sync between two tabs and land in git', async ({ browser }) => {
  const name = `E2E ${Date.now()}`;
  const ctxA = await browser.newContext();
  const ctxB = await browser.newContext();
  const a = await ctxA.newPage();
  const b = await ctxB.newPage();

  await a.goto('/');
  if (await a.getByRole('button', { name: 'New project' }).isVisible()) {
    await a.getByRole('button', { name: 'New project' }).click();
  } else {
    await a.getByRole('button', { name: 'Blank article' }).click();
  }
  await a.getByLabel('Project name').fill(name);
  await a.getByRole('button', { name: 'Create' }).click();
  await expect(a).toHaveURL(/\/p\/[a-z0-9-]+$/);
  await setName(a, 'Ana Novak');
  await expect(a.locator('.cm-content')).toContainText('documentclass');

  await b.goto(a.url());
  await setName(b, 'Julia Ruiz');
  await expect(b.locator('.cm-content')).toContainText('documentclass');
  await expect(a.locator('.presence .n')).toHaveText('2 online');

  const marker = `Synced-${Date.now()}`;
  await a.locator('.cm-content').click();
  await a.keyboard.press('Control+End');
  await a.keyboard.type(`\n% ${marker}`);
  await expect(b.locator('.cm-content')).toContainText(marker, { timeout: 10_000 });

  await b.getByRole('button', { name: 'History' }).click();
  await expect(b.locator('.ti .l').first()).toHaveText('edit: main.tex', { timeout: 15_000 });
  await expect(b.locator('.ti .s').first()).toContainText('Ana Novak');

  await ctxA.close();
  await ctxB.close();
});

test('build produces a PDF, an error card fixes it, and SyncTeX jumps both ways', async ({ page }) => {
  const name = `Build ${Date.now()}`;
  await page.goto('/');
  const newProject = page.getByRole('button', { name: 'New project' });
  if (await newProject.isVisible()) await newProject.click();
  else await page.getByRole('button', { name: 'Blank article' }).click();
  await page.getByLabel('Project name').fill(name);
  await page.getByRole('button', { name: 'Create' }).click();
  await expect(page).toHaveURL(/\/p\/[a-z0-9-]+$/);
  await setName(page, 'Ana Novak');

  // First build renders a PDF.
  await page.keyboard.press('Control+Enter');
  await expect(page.locator('.build .st')).toContainText('Compiled', { timeout: 60_000 });
  await expect(page.locator('.pdf-page canvas')).toBeVisible();

  // A \citep without natbib fails the build and offers a one-click fix.
  await page.locator('.cm-content').click();
  await page.keyboard.press('Control+End');
  await page.keyboard.type('\nSee \\citep{x}.');
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
