import { expect, test, type Page } from '@playwright/test';
import { authenticate } from './auth';

async function create(page: Page, name: string) {
  const csrf = (await page.context().cookies()).find((c) => c.name === 'galley_csrf')!.value;
  const response = await page.request.post('/api/projects', { headers: { 'x-csrf-token': csrf }, data: { name } });
  expect(response.status()).toBe(201);
  return await response.json() as { id: string; name: string };
}

test('project links support modified clicks, middle clicks, keyboard and separate rename', async ({ page, context }) => {
  await authenticate(page);
  const project = await create(page, `Navigation ${Date.now()}`);
  await page.reload();
  const card = page.getByRole('article', { name: project.name, exact: true });
  const link = card.getByRole('link', { name: `Open ${project.name}`, exact: true });
  await expect(link).toHaveAttribute('href', `/p/${project.id}`);
  for (const options of [{ modifiers: ['Control'] as ['Control'] }, { button: 'middle' as const }]) {
    const opened = context.waitForEvent('page');
    await link.click(options);
    const child = await opened;
    await expect(child).toHaveURL(new RegExp(`/p/${project.id}$`));
    await expect(page).not.toHaveURL(/\/p\//);
    await child.close();
  }
  await card.getByRole('button', { name: 'Rename', exact: true }).click();
  await card.getByRole('textbox', { name: 'Project name', exact: true }).fill(`${project.name} renamed`);
  await card.getByRole('button', { name: 'Save', exact: true }).click();
  const renamed = page.getByRole('link', { name: `Open ${project.name} renamed`, exact: true });
  await expect(renamed).toBeVisible();
  await expect(page).not.toHaveURL(/\/p\//);
  await renamed.focus();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(new RegExp(`/p/${project.id}$`));
  await page.goto('/');
  await page.getByRole('link', { name: `Open ${project.name} renamed`, exact: true }).click({ clickCount: 3 });
  await expect(page).toHaveURL(new RegExp(`/p/${project.id}$`));
});

test('project deletion confirms, preserves failures, removes the project and survives reload', async ({ page }) => {
  await authenticate(page);
  const project = await create(page, `Delete ${Date.now()}`);
  await page.reload();
  const card = page.getByRole('article', { name: project.name, exact: true });
  await card.getByRole('button', { name: 'Delete', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Delete project', exact: true });
  await expect(dialog).toContainText(project.name);
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(card).toBeVisible();

  await page.route(`**/api/projects/${project.id}`, (route) => route.request().method() === 'DELETE'
    ? route.fulfill({ status: 500, json: { error: 'Temporary deletion failure. Try again.' } }) : route.continue());
  await card.getByRole('button', { name: 'Delete', exact: true }).click();
  await dialog.getByRole('button', { name: 'Delete project', exact: true }).click();
  await expect(dialog.getByRole('alert')).toContainText('Try again');
  await expect(card).toBeAttached();
  await page.unroute(`**/api/projects/${project.id}`);
  await dialog.getByRole('button', { name: 'Delete project', exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(card).toHaveCount(0);
  const response = await page.request.get(`/api/projects/${project.id}`);
  expect([403, 404]).toContain(response.status());
  await page.reload();
  await expect(page.getByRole('button', { name: 'Refresh projects', exact: true })).toBeVisible();
  await expect(card).toHaveCount(0);
});

// A complete tiny PDF fixture: the test exercises PDF.js rendering, never starts a compiler.
function samplePdf(): Buffer {
  const stream = 'BT /F1 24 Tf 50 720 Td (A recognizable project preview) Tj ET';
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
    `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
  ];
  let text = '%PDF-1.4\n';
  const offsets = [0];
  for (let i = 0; i < objects.length; i++) { offsets.push(text.length); text += `${i + 1} 0 obj\n${objects[i]}\nendobj\n`; }
  const xref = text.length;
  text += `xref\n0 6\n0000000000 65535 f \n${offsets.slice(1).map((offset) => `${String(offset).padStart(10, '0')} 00000 n \n`).join('')}trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(text);
}

test('optional previews show the saved PDF, handle no PDF, persist and fit mobile', async ({ page }, testInfo) => {
  await authenticate(page);
  const saved = await create(page, `Preview ${Date.now()}`);
  const empty = await create(page, `Empty preview ${Date.now()}`);
  let builds = 0;
  page.on('request', (request) => { if (request.method() === 'POST' && /\/build$/.test(request.url())) builds++; });
  await page.route(`**/api/projects/${saved.id}/build/pdf?*`, (route) => route.fulfill({ contentType: 'application/pdf', body: samplePdf() }));
  await page.reload();
  const card = page.getByRole('article', { name: saved.name, exact: true });
  await expect(card.getByRole('img', { name: 'First page of the last compiled PDF' })).toBeVisible();
  const pixels = await card.locator('canvas').evaluate((canvas: HTMLCanvasElement) => {
    const data = canvas.getContext('2d')!.getImageData(0, 0, canvas.width, canvas.height).data;
    return data.some((n, i) => i % 4 !== 3 && n < 128);
  });
  expect(pixels).toBe(true);
  await expect(page.getByRole('article', { name: empty.name, exact: true })).toContainText('Build once to show a preview');
  await page.getByRole('checkbox', { name: 'Show previews', exact: true }).uncheck();
  await expect(page.locator('.project-thumbnail')).toHaveCount(0);
  await page.reload();
  await expect(page.getByRole('checkbox', { name: 'Show previews', exact: true })).not.toBeChecked();
  await expect(page.locator('.project-thumbnail')).toHaveCount(0);
  await page.getByRole('checkbox', { name: 'Show previews', exact: true }).check();
  await expect(card.locator('canvas')).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath('projects-desktop.png'), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(card).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath('projects-mobile.png'), fullPage: true });
  expect(builds).toBe(0);
});
