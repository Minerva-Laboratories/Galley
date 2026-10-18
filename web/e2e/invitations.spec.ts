import { expect, test } from '@playwright/test';
import { authenticate, dismissTour } from './auth';

test('an invitation appears on an already open recipient dashboard', async ({ page, context, browser }) => {
  await authenticate(page);
  const csrf = (await context.cookies()).find((cookie) => cookie.name === 'galley_csrf')!.value;
  const name = `Invitation ${Date.now()}`;
  const created = await page.request.post('/api/projects', {
    headers: { 'x-csrf-token': csrf }, data: { name },
  });
  expect(created.status()).toBe(201);
  const { id } = await created.json();

  // A distinct account stays on its project list while the owner grants access elsewhere.
  // The E2E server must use server.public_signup=true; all addresses are synthetic .test accounts.
  const recipient = await browser.newContext({ baseURL: new URL(page.url()).origin });
  try {
    const dashboard = await recipient.newPage();
    const email = `invitation-${Date.now()}@galley.test`;
    const signedUp = await dashboard.request.post('/api/auth/signup', {
      data: { email, name: 'Invited collaborator', password: 'GalleyE2Epass123!' },
    });
    expect(signedUp.ok(), 'Invitation E2E fixtures need server.public_signup=true').toBe(true);
    await dashboard.goto('/');
    await expect(dashboard.locator('.empty')).toContainText('Create a project');
    await expect(dashboard.locator('.pcard')).toHaveCount(0);

    await page.goto(`/p/${id}`);
    await dismissTour(page);
    await page.getByRole('button', { name: 'Share', exact: true }).click();
    const share = page.getByRole('dialog', { name: 'Share project' });
    await expect(share.getByText('no email is sent.', { exact: false })).toBeVisible();
    await share.getByLabel('Invite email').fill(email.toUpperCase());
    await share.getByLabel('Invite role').selectOption('admin');
    await share.getByRole('button', { name: 'Invite', exact: true }).click();
    await expect(share.getByRole('status')).toContainText('Access granted');
    await expect(share.locator('.member').filter({ hasText: email })).toHaveCount(1);

    // No navigation or reload on the recipient side: membership must become visible live.
    const project = dashboard.locator('.pcard').filter({ hasText: name });
    await expect(project).toBeVisible({ timeout: 15_000 });
    await project.click();
    await expect(dashboard).toHaveURL(new RegExp(`/p/${id}$`));
    await dismissTour(dashboard);

    // Retrying the invite is harmless and leaves a single member row.
    await share.getByLabel('Invite email').fill(email);
    await share.getByRole('button', { name: 'Invite', exact: true }).click();
    await expect(share.getByRole('status')).toContainText('Access granted');
    await expect(share.locator('.member').filter({ hasText: email })).toHaveCount(1);

    // With two admins and unanimous governance, the same UI must explain why a third
    // person's invitation has not granted access yet, then update after the other admin votes.
    const governanceChanged = page.waitForResponse((response) => response.url().endsWith(`/api/projects/${id}/governance`) && response.request().method() === 'PUT');
    await share.getByLabel('Role change approval').selectOption('unanimous');
    expect((await governanceChanged).ok()).toBe(true);
    await expect(share.getByLabel('Role change approval')).toHaveValue('unanimous');
    const third = await browser.newContext({ baseURL: new URL(page.url()).origin });
    try {
      const pendingEmail = `pending-${Date.now()}@galley.test`;
      const signup = await third.request.post('/api/auth/signup', {
        data: { email: pendingEmail, name: 'Pending collaborator', password: 'GalleyE2Epass123!' },
      });
      expect(signup.ok()).toBe(true);
      await share.getByLabel('Invite email').fill(pendingEmail);
      await share.getByLabel('Invite role').selectOption('viewer');
      await share.getByRole('button', { name: 'Invite', exact: true }).click();
      await expect(share.getByRole('status')).toContainText('Invitation pending');
      await expect(share.getByText('Add Pending collaborator as viewer', { exact: true })).toBeVisible();
      const beforeApproval = await third.request.get('/api/projects');
      expect(await beforeApproval.json()).toEqual([]);
      const governance = await recipient.request.get(`/api/projects/${id}/governance`);
      const { requests } = await governance.json();
      const secondCsrf = (await recipient.cookies()).find((cookie) => cookie.name === 'galley_csrf')!.value;
      const approved = await recipient.request.post(`/api/projects/${id}/requests/${requests[0].id}/vote`, {
        headers: { 'x-csrf-token': secondCsrf }, data: { approve: true },
      });
      expect(approved.ok()).toBe(true);
      await expect(share.locator('.member').filter({ hasText: pendingEmail })).toHaveCount(1);
      await expect(share.getByText('Invitation pending:', { exact: false })).toHaveCount(0);
      const afterApproval = await third.request.get('/api/projects');
      expect((await afterApproval.json()).map((project: { id: string }) => project.id)).toContain(id);
    } finally {
      await third.close();
    }
  } finally {
    await recipient.close();
  }
});
