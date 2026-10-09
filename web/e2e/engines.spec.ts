import { expect, test, type Page } from '@playwright/test';
import { authenticate, dismissTour, startNewProject } from './auth';

const inventory = {
  selected: 'tectonic',
  engines: [
    { engine: 'tectonic', available: true, version: '0.15', reason: null },
    { engine: 'pdflatex', available: true, version: 'TeX Live', reason: 'BibTeX is not installed; bibliography runs may fail.' },
    { engine: 'xelatex', available: false, version: null, reason: 'XeLaTeX is not installed on this server.' },
    { engine: 'lualatex', available: false, version: null, reason: 'LuaLaTeX is not installed on this server.' },
    { engine: 'latex', available: false, version: null, reason: 'LaTeX is not installed on this server.' },
  ],
};

function alternativePdfEngine(availability: { selected: string; engines: { engine: string; available: boolean }[] }) {
  const alternative = availability.engines.find((item) => item.available && item.engine !== availability.selected && item.engine !== 'latex');
  if (!alternative && process.env.GALLEY_TEST_ENGINE_MATRIX === '1') {
    throw new Error('The strict engine matrix requires another available PDF compiler.');
  }
  return alternative;
}

async function createProject(page: Page): Promise<string> {
  await authenticate(page);
  await startNewProject(page);
  await page.getByLabel('Project name').fill(`Engines ${Date.now()}`);
  await page.getByRole('button', { name: 'Create' }).click();
  await expect(page).toHaveURL(/\/p\/[a-z0-9-]+$/);
  const name = page.getByLabel('Your name, as collaborators see it');
  if (await name.isVisible()) {
    await name.fill('Engine Tester');
    await page.getByRole('button', { name: 'Save' }).click();
  }
  await dismissTour(page);
  return page.url().split('/').pop()!;
}

test('compiler selection shows availability, saves only on success, and respects role', async ({ page, browser }) => {
  const id = await createProject(page);
  const meta = await (await page.request.get(`/api/projects/${id}`)).json();
  await page.route(`**/api/projects/${id}/engines`, (route) => route.fulfill({ json: inventory }));
  let fail = true;
  let writes = 0;
  await page.route(`**/api/projects/${id}/settings`, async (route) => {
    if (route.request().method() !== 'PATCH') return route.continue();
    writes += 1;
    expect(route.request().postDataJSON().engine).toBe('pdflatex');
    if (fail) return route.fulfill({ status: 503, json: { error: 'Compiler setting could not be saved.' } });
    return route.fulfill({ json: { ...meta, engine: 'pdflatex' } });
  });
  await page.reload();
  const compiler = page.getByRole('combobox', { name: 'Compiler' });
  await expect(compiler).toHaveValue('tectonic');
  await expect(compiler.locator('option[value="xelatex"]')).toBeDisabled();
  await page.locator('.engine-reasons summary').click();
  await expect(page.locator('.engine-reasons-list')).toContainText('XeLaTeX is not installed');
  await expect(page.locator('.engine-reasons-list')).toContainText('BibTeX is not installed');
  await compiler.selectOption('pdflatex');
  await expect(page.locator('.toast')).toContainText('Compiler setting could not be saved');
  await expect(compiler).toHaveValue('tectonic');
  fail = false;
  await compiler.selectOption('pdflatex');
  await expect(compiler).toHaveValue('pdflatex');
  await expect(page.locator('.build .eng')).toContainText('Next: pdfLaTeX');
  await expect(page.locator('.compiler-control .engine-note')).toContainText('BibTeX is not installed');
  expect(writes).toBe(2);

  const readonly = await browser.newPage();
  await authenticate(readonly);
  await readonly.route(`**/api/projects/${id}/engines`, (route) => route.fulfill({ json: { ...inventory, selected: 'pdflatex' } }));
  await readonly.route(`**/api/projects/${id}`, async (route) => {
    if (route.request().method() === 'GET') return route.fulfill({ json: { ...meta, engine: 'pdflatex', role: 'viewer' } });
    return route.continue();
  });
  await readonly.goto(page.url());
  await expect(readonly.locator('.build .eng')).toContainText('Next: pdfLaTeX');
  await expect(readonly.getByRole('combobox', { name: 'Compiler' })).toHaveCount(0);
  await readonly.close();
});

test('engine changes propagate to another tab', async ({ browser }) => {
  const context = await browser.newContext();
  const a = await context.newPage();
  const id = await createProject(a);
  const availability = await (await a.request.get(`/api/projects/${id}/engines`)).json();
  const alternative = alternativePdfEngine(availability);
  if (!alternative) { test.skip(true, 'Requires a second installed compiler'); return; }
  const b = await context.newPage();
  await authenticate(b);
  await b.goto(a.url());
  await expect(b.getByRole('combobox', { name: 'Compiler' })).toBeEnabled({ timeout: 30_000 });
  await expect(b.getByRole('combobox', { name: 'Compiler' })).toHaveValue(availability.selected);
  await a.getByRole('combobox', { name: 'Compiler' }).selectOption(alternative.engine);
  await expect(b.getByRole('combobox', { name: 'Compiler' })).toHaveValue(alternative.engine, { timeout: 15_000 });
  await expect(b.locator('.build .eng')).toContainText('Next:');
  await context.close();
});

test('temporary builds and failed builds preserve the selected engine and visible PDF producer', async ({ page }) => {
  test.setTimeout(240_000);
  const id = await createProject(page);
  const availability = await (await page.request.get(`/api/projects/${id}/engines`)).json();
  const alternative = alternativePdfEngine(availability);
  if (!alternative) { test.skip(true, 'Requires a second installed compiler'); return; }
  await page.getByRole('combobox', { name: 'Build mode' }).selectOption('manual');
  await page.locator('.cm-content').click();
  await page.keyboard.press('Control+A');
  await page.keyboard.insertText('\\documentclass{article}\n\\begin{document}\nCompiler test.\n\\end{document}\n');
  await expect(page.locator('.cm-content')).toContainText('Compiler test.');
  await page.locator('.build .primary').click();
  await expect(page.locator('.build .st')).toContainText('Compiled', { timeout: 75_000 });
  await expect(page.locator('.pdf-producer')).toContainText('PDF:');
  const originalProducer = await page.locator('.pdf-producer').innerText();
  const selected = await page.getByRole('combobox', { name: 'Compiler' }).inputValue();

  await page.evaluate(async ({ projectId, engine }) => {
    const csrf = /(?:^|;\s*)galley_csrf=([^;]+)/.exec(document.cookie)?.[1];
    const response = await fetch(`/api/projects/${projectId}/build`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', ...(csrf ? { 'x-csrf-token': decodeURIComponent(csrf) } : {}) },
      body: JSON.stringify({ draft: false, engine }),
    });
    if (!response.ok) throw new Error(`Temporary build returned ${response.status}`);
  }, { projectId: id, engine: alternative.engine });
  await expect.poll(async () => {
    const status = await (await page.request.get(`/api/projects/${id}/build`)).json();
    return status.last?.engine === alternative.engine ? status.last?.pdf_engine : null;
  }, { timeout: 75_000 }).toBe(alternative.engine);
  await expect(page.getByRole('combobox', { name: 'Compiler' })).toHaveValue(selected);
  await expect(page.locator('.build .eng')).toContainText('Next:');
  await expect(page.locator('.pdf-producer')).not.toHaveText(originalProducer);
  const producerAfterTemp = await page.locator('.pdf-producer').innerText();

  await page.locator('.cm-line').filter({ hasText: '\\end{document}' }).first().click();
  await page.keyboard.press('Home');
  await page.keyboard.type('\\undefinedEngineTest\n');
  await expect(page.locator('.cm-content')).toContainText('\\undefinedEngineTest');
  await page.locator('.build .primary').click();
  await expect.poll(async () => {
    const status = await (await page.request.get(`/api/projects/${id}/build`)).json();
    return status.last?.engine === selected ? status.last?.status : null;
  }, { timeout: 75_000 }).toBe('failed');
  await expect(page.locator('.build .st')).toContainText('Build failed');
  await expect(page.locator('.pdf-producer')).toHaveText(producerAfterTemp || originalProducer);
  await expect(page.getByRole('combobox', { name: 'Compiler' })).toHaveValue(selected);
});
