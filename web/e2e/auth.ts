import { expect, type Page } from '@playwright/test';

const email = process.env.GALLEY_E2E_EMAIL ?? 'e2e-admin@galley.test';
const password = process.env.GALLEY_E2E_PASSWORD ?? 'GalleyE2Epass123!';

async function signIn(page: Page): Promise<void> {
  for (let attempt = 0; attempt < 5; attempt++) {
    const response = await page.request.post('/api/auth/login', { data: { email, password } });
    if (response.ok()) return;
    if (attempt === 4) throw new Error(`Could not sign in for E2E tests (${response.status()}).`);
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
}

/** Authenticate each browser context against the fresh server used by scripts/e2e.sh. */
export async function authenticate(page: Page): Promise<void> {
  const meResponse = await page.request.get('/api/auth/me');
  if (!meResponse.ok()) throw new Error(`Could not check Galley authentication (${meResponse.status()}).`);
  const me = await meResponse.json() as { user: unknown | null; needs_setup: boolean };
  if (!me.user) {
    if (me.needs_setup) {
      const created = await page.request.post('/api/auth/signup', {
        data: { email, name: 'E2E Admin', password },
      });
      // Concurrent test workers may both see first-run setup. One creates the account;
      // the other signs into that same account after its signup loses the race.
      if (!created.ok()) await signIn(page);
    } else {
      await signIn(page);
    }
  }
  await page.goto('/');
  await expect(page.locator('.projects')).toBeVisible();
}

/** Open the new-project form. An account with projects shows "New project"; an empty one shows the
 * template cards instead. Wait until the list has loaded and one of them is on screen, then use it:
 * checking before the list loads picks a control that is about to disappear. */
export async function startNewProject(page: Page): Promise<void> {
  const newProject = page.getByRole('button', { name: 'New project' });
  const blank = page.getByRole('button', { name: 'Blank article' });
  await expect(newProject.or(blank).first()).toBeVisible();
  if (await newProject.isVisible()) await newProject.click();
  else await blank.click();
}

/** First project visit opens the tour after the editor mounts; close it through its own control. */
export async function dismissTour(page: Page): Promise<void> {
  await expect(page.locator('.cm-content')).toBeVisible();
  const tour = page.getByRole('dialog', { name: 'Welcome tour' });
  try {
    await tour.waitFor({ state: 'visible', timeout: 2_000 });
  } catch {
    // This browser context has already completed the tour.
  }
  if (await tour.isVisible()) {
    await tour.getByRole('button', { name: 'Skip tour' }).click();
    await expect(tour).toBeHidden();
  }
}
