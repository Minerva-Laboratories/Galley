import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test';
import { dismissTour } from './auth';

// Live access checks (SPEC §10.2): an open project notices a removal, a lowered role and a sign-out
// without a reload. Every test uses fresh synthetic .test accounts, so the E2E server must use
// server.public_signup=true, as scripts/e2e.sh configures.

const PASSWORD = 'GalleyE2Epass123!';
let serial = 0;

interface Account {
  context: BrowserContext;
  page: Page;
  email: string;
  id: string;
  csrf: string;
}

async function csrfOf(context: BrowserContext): Promise<string> {
  return (await context.cookies()).find((cookie) => cookie.name === 'galley_csrf')!.value;
}

async function account(browser: Browser, baseURL: string, name: string): Promise<Account> {
  const context = await browser.newContext({ baseURL });
  const page = await context.newPage();
  const email = `access-${Date.now()}-${serial++}@galley.test`;
  const signedUp = await page.request.post('/api/auth/signup', { data: { email, name, password: PASSWORD } });
  expect(signedUp.ok(), 'Access E2E fixtures need server.public_signup=true').toBe(true);
  const { user } = await signedUp.json();
  return { context, page, email, id: user.id, csrf: await csrfOf(context) };
}

async function createProject(owner: Account, name: string): Promise<string> {
  const created = await owner.page.request.post('/api/projects', { headers: { 'x-csrf-token': owner.csrf }, data: { name } });
  expect(created.status()).toBe(201);
  return (await created.json()).id;
}

async function invite(owner: Account, id: string, member: Account, role: string) {
  const res = await owner.page.request.post(`/api/projects/${id}/members`, {
    headers: { 'x-csrf-token': owner.csrf }, data: { email: member.email, role },
  });
  expect(res.ok()).toBe(true);
  expect((await res.json()).status).toBe('applied');
}

async function setRole(owner: Account, id: string, member: Account, role: string) {
  const res = await owner.page.request.put(`/api/projects/${id}/members/${member.id}`, {
    headers: { 'x-csrf-token': owner.csrf }, data: { role },
  });
  expect(res.ok()).toBe(true);
  expect((await res.json()).status).toBe('applied');
}

async function removeMember(owner: Account, id: string, member: Account) {
  const res = await owner.page.request.delete(`/api/projects/${id}/members/${member.id}`, {
    headers: { 'x-csrf-token': owner.csrf },
  });
  expect(res.ok()).toBe(true);
  expect((await res.json()).status).toBe('applied');
}

function databases(page: Page): Promise<string[]> {
  return page.evaluate(async () => (await indexedDB.databases()).map((d) => d.name ?? ''));
}

/** Wait until the project's offline copy of main.tex exists in this browser. */
async function expectOfflineCopy(page: Page, id: string) {
  await expect.poll(() => databases(page)).toContain(`galley:${id}:main.tex`);
}

async function expectNoOfflineCopy(page: Page, id: string) {
  await expect.poll(async () => (await databases(page)).filter((n) => n.startsWith(`galley:${id}:`)), { timeout: 10_000 }).toEqual([]);
}

/** Track every /ws/ socket the page opens, and which ones are still open. */
function trackSockets(page: Page) {
  const open = new Set<object>();
  let opened = 0;
  page.on('websocket', (ws) => {
    if (!ws.url().includes('/ws/')) return;
    opened++;
    open.add(ws);
    ws.on('close', () => open.delete(ws));
  });
  return { open, opened: () => opened };
}

const toast = (page: Page, text: string) => page.locator('.toast', { hasText: text });
/** The signed-out screen: sign in, or create an account where signup is open. */
const signInScreen = (page: Page) => page.locator('.auth-card');

async function openProject(member: Account, id: string) {
  await member.page.goto(`/p/${id}`);
  await dismissTour(member.page);
}

test.describe('live access checks', () => {
  let contexts: BrowserContext[] = [];
  let base = '';
  const make = async (browser: Browser, name: string) => {
    const made = await account(browser, base, name);
    contexts.push(made.context);
    return made;
  };
  test.beforeEach(({ baseURL }) => {
    base = baseURL!;
    contexts = [];
  });
  test.afterEach(async () => {
    for (const context of contexts) await context.close();
  });

  test('a member removed while connected leaves the project and its offline copies', async ({ browser }) => {
    const owner = await make(browser, 'Owner');
    const member = await make(browser, 'Removed member');
    const name = `Access removed ${Date.now()}`;
    const id = await createProject(owner, name);
    await invite(owner, id, member, 'editor');
    const sockets = trackSockets(member.page);
    await openProject(member, id);
    await expectOfflineCopy(member.page, id);
    await expect.poll(() => sockets.open.size).toBeGreaterThanOrEqual(2);

    await removeMember(owner, id, member);
    await expect(toast(member.page, 'You no longer have access to this project.')).toBeVisible();
    await expect(member.page).toHaveURL(/\/$/);
    await expect(member.page.locator('.projects')).toBeVisible();
    await expect(member.page.locator('.pcard').filter({ hasText: name })).toHaveCount(0);
    await expectNoOfflineCopy(member.page, id);

    // No socket is opened again afterwards.
    await expect.poll(() => sockets.open.size).toBe(0);
    const after = sockets.opened();
    await member.page.waitForTimeout(3000);
    expect(sockets.opened()).toBe(after);
  });

  test('an admin lowered to viewer loses the Share dialog and editing in place', async ({ browser }) => {
    const owner = await make(browser, 'Owner');
    const member = await make(browser, 'Lowered admin');
    const id = await createProject(owner, `Access lowered ${Date.now()}`);
    await invite(owner, id, member, 'admin');
    await openProject(member, id);
    const editor = member.page.locator('.cm-content');
    await expect(editor).toHaveAttribute('contenteditable', 'true');
    await member.page.getByRole('button', { name: 'Share', exact: true }).click();
    const share = member.page.getByRole('dialog', { name: 'Share project' });
    await expect(share).toBeVisible();

    await setRole(owner, id, member, 'viewer');
    await expect(share).toBeHidden();
    await expect(editor).toHaveAttribute('contenteditable', 'false');
    const notice = toast(member.page, 'Your role is now viewer. Editing is off.');
    await expect(notice).toBeVisible();
    await expect(notice).toBeHidden({ timeout: 6_000 });
    await expect(member.page).toHaveURL(new RegExp(`/p/${id}$`));

    // Raised back to editor, the same open editor accepts edits again.
    await setRole(owner, id, member, 'editor');
    await expect(toast(member.page, 'Your role is now editor. You can edit now.')).toBeVisible();
    await expect(editor).toHaveAttribute('contenteditable', 'true');
  });

  test('a member removed while away is told on return', async ({ browser }) => {
    const owner = await make(browser, 'Owner');
    const member = await make(browser, 'Away member');
    const id = await createProject(owner, `Access away ${Date.now()}`);
    await invite(owner, id, member, 'editor');
    await member.page.clock.install();
    const sockets = trackSockets(member.page);
    await openProject(member, id);
    await expectOfflineCopy(member.page, id);
    await expect.poll(() => sockets.open.size).toBeGreaterThanOrEqual(2);

    await member.page.clock.fastForward('31:00');
    await expect(member.page.locator('.pill', { hasText: 'Paused while you were away' })).toBeVisible();
    await expect.poll(() => sockets.open.size, { message: 'every socket is closed while away', timeout: 30_000 }).toBe(0);

    await removeMember(owner, id, member);
    const probe = member.page.waitForResponse((r) =>
      r.url().endsWith(`/api/projects/${id}`) && r.request().method() === 'GET' && r.status() === 404);
    await member.page.locator('.cm-content').click();
    await probe;
    await expect(toast(member.page, 'You no longer have access to this project.')).toBeVisible();
    await expect(member.page).toHaveURL(/\/$/);
    await expectNoOfflineCopy(member.page, id);
  });

  test('a role lowered while away applies on return', async ({ browser }) => {
    const owner = await make(browser, 'Owner');
    const member = await make(browser, 'Away editor');
    const id = await createProject(owner, `Access away role ${Date.now()}`);
    await invite(owner, id, member, 'editor');
    await member.page.clock.install();
    const sockets = trackSockets(member.page);
    await openProject(member, id);
    const editor = member.page.locator('.cm-content');
    await expect(editor).toHaveAttribute('contenteditable', 'true');
    await expect.poll(() => sockets.open.size).toBeGreaterThanOrEqual(2);

    await member.page.clock.fastForward('31:00');
    await expect(member.page.locator('.pill', { hasText: 'Paused while you were away' })).toBeVisible();
    await expect.poll(() => sockets.open.size, { message: 'every socket is closed while away', timeout: 30_000 }).toBe(0);

    await setRole(owner, id, member, 'viewer');
    await editor.click();
    await expect(member.page.locator('.pill', { hasText: 'Paused while you were away' })).toBeHidden();
    await expect(editor).toHaveAttribute('contenteditable', 'false');
  });

  test('signing out one session tells its other tabs and leaves other sessions connected', async ({ browser }) => {
    const user = await make(browser, 'Two sessions');
    const id = await createProject(user, `Access sign-out ${Date.now()}`);
    const a1 = user.page;
    const a2 = await user.context.newPage();
    await a1.goto(`/p/${id}`);
    await dismissTour(a1);
    await a2.goto(`/p/${id}`);
    await expect(a2.locator('.cm-content')).toBeVisible();

    // A second session of the same account, in its own browser context.
    const other = await browser.newContext({ baseURL: base });
    contexts.push(other);
    const b = await other.newPage();
    const loggedIn = await b.request.post('/api/auth/login', { data: { email: user.email, password: PASSWORD } });
    expect(loggedIn.ok()).toBe(true);
    await b.goto(`/p/${id}`);
    await dismissTour(b);
    await expect(b.locator('.presence .n')).toHaveText('Only you');

    const out = await a1.request.post('/api/auth/logout', { headers: { 'x-csrf-token': user.csrf } });
    expect(out.ok()).toBe(true);
    await expect(toast(a2, 'You were signed out.')).toBeVisible();
    await expect(signInScreen(a2)).toBeVisible();

    // The other session keeps its connection: it hears a new member arrive.
    const joiner = await make(browser, 'Joiner');
    const userAsOwner = { ...user, page: b, csrf: await csrfOf(other) };
    await invite(userAsOwner, id, joiner, 'viewer');
    await openProject(joiner, id);
    await expect(b.locator('.presence .n')).toHaveText('2 online');
    await expect(toast(b, 'You were signed out.')).toHaveCount(0);
    await expect(b.locator('.cm-content')).toBeVisible();
  });

  test('Sign out deletes this account\'s offline copies and keeps another account\'s', async ({ browser }) => {
    const x = await make(browser, 'Signing out');
    const id = await createProject(x, `Access copies ${Date.now()}`);
    await openProject(x, id);
    await expectOfflineCopy(x.page, id);
    // Another account's copy in the same browser.
    await x.page.evaluate(async () => {
      await new Promise<void>((resolve, reject) => {
        const req = indexedDB.open('galley:other:main.tex');
        req.onsuccess = () => {
          req.result.close();
          resolve();
        };
        req.onerror = () => reject(req.error);
      });
      localStorage.setItem('galley:idb-owner:other', 'another-account');
    });

    await x.page.goto('/');
    await x.page.getByRole('button', { name: 'Sign out' }).click();
    await expect(signInScreen(x.page)).toBeVisible();
    await expect(toast(x.page, 'You were signed out.')).toHaveCount(0);
    await expectNoOfflineCopy(x.page, id);
    expect(await databases(x.page)).toContain('galley:other:main.tex');
    expect(await x.page.evaluate(() => localStorage.getItem('galley:idb-owner:other'))).toBe('another-account');
  });

  test('opening a project after removal deletes its offline copies', async ({ browser }) => {
    const owner = await make(browser, 'Owner');
    const member = await make(browser, 'Later removed');
    const id = await createProject(owner, `Access direct ${Date.now()}`);
    await invite(owner, id, member, 'editor');
    await openProject(member, id);
    await expectOfflineCopy(member.page, id);
    await member.page.goto('/');
    await expect(member.page.locator('.projects')).toBeVisible();
    await expectOfflineCopy(member.page, id);

    await removeMember(owner, id, member);
    await member.page.goto(`/p/${id}`);
    await expect(member.page.getByRole('button', { name: 'Back to projects' })).toBeVisible();
    await expectNoOfflineCopy(member.page, id);
  });

  test('another account opening a project it cannot see keeps the first account\'s offline copies', async ({ browser }) => {
    const first = await make(browser, 'First account');
    const second = await make(browser, 'Second account');
    const id = await createProject(first, `Access other account ${Date.now()}`);
    await openProject(first, id);
    await expectOfflineCopy(first.page, id);
    await first.page.goto('/');
    await expect(first.page.locator('.projects')).toBeVisible();
    expect(await first.page.evaluate((key) => localStorage.getItem(key), `galley:idb-owner:${id}`)).toBe(first.id);

    // The second account signs in in the same browser, without the first one's Sign out, and
    // follows the first account's project link from history.
    const loggedIn = await first.page.request.post('/api/auth/login', { data: { email: second.email, password: PASSWORD } });
    expect(loggedIn.ok()).toBe(true);
    const refused = first.page.waitForResponse((r) =>
      r.url().endsWith(`/api/projects/${id}`) && r.request().method() === 'GET' && r.status() === 404);
    await first.page.goto(`/p/${id}`);
    await refused;
    await expect(first.page.getByRole('button', { name: 'Back to projects' })).toBeVisible();

    // A deletion would finish within a few seconds; the copy and its owner record stay put.
    await first.page.waitForTimeout(4000);
    expect(await databases(first.page)).toContain(`galley:${id}:main.tex`);
    expect(await first.page.evaluate((key) => localStorage.getItem(key), `galley:idb-owner:${id}`)).toBe(first.id);
  });

  test('a guest removed by revoking their share link ends on the sign-in screen', async ({ browser }) => {
    const owner = await make(browser, 'Owner');
    const id = await createProject(owner, `Access revoked guest ${Date.now()}`);
    const created = await owner.page.request.post(`/api/projects/${id}/shares`, {
      headers: { 'x-csrf-token': owner.csrf }, data: { role: 'editor', expires_days: 7 },
    });
    expect(created.status()).toBe(201);
    const link: { id: string; path: string } = await created.json();

    // The guest joins through the link in a browser of their own, with no account.
    const context = await browser.newContext({ baseURL: base });
    contexts.push(context);
    const guest = await context.newPage();
    const joined = await guest.request.post('/api/auth/landing', { data: { token: link.path.replace('/s/', ''), name: 'Link guest' } });
    expect(joined.ok()).toBe(true);
    expect((await joined.json()).user.is_guest).toBe(true);
    const sockets = trackSockets(guest);
    await guest.goto(`/p/${id}`);
    await dismissTour(guest);
    await expect(guest.locator('.cm-content')).toBeVisible();
    await expectOfflineCopy(guest, id);
    await expect.poll(() => sockets.open.size).toBeGreaterThanOrEqual(2);

    const revoked = await owner.page.request.delete(`/api/projects/${id}/shares/${link.id}`, { headers: { 'x-csrf-token': owner.csrf } });
    expect(revoked.ok()).toBe(true);
    expect((await revoked.json()).removed_guests).toBe(1);

    await expect(toast(guest, 'You no longer have access to this project.')).toBeVisible();
    await expect(guest).toHaveURL(/\/$/);
    await expect(signInScreen(guest)).toBeVisible();
    await expect(guest.getByText('Sign in to continue.')).toHaveCount(0);
    await expect(guest.locator('.projects')).toHaveCount(0);
    await expectNoOfflineCopy(guest, id);
    // It stays there: the guest's session is gone on the server too.
    await guest.waitForTimeout(1500);
    await expect(signInScreen(guest)).toBeVisible();
  });

  test('the Share dialog lists guests under their link, removes one, and a revoked link says so', async ({ browser }) => {
    const owner = await make(browser, 'Owner');
    const id = await createProject(owner, `Access link guests ${Date.now()}`);
    const created = await owner.page.request.post(`/api/projects/${id}/shares`, {
      headers: { 'x-csrf-token': owner.csrf }, data: { role: 'commenter', expires_days: 7 },
    });
    expect(created.status()).toBe(201);
    const link: { path: string } = await created.json();
    const token = link.path.replace('/s/', '');
    for (const name of ['Guest One', 'Guest Two']) {
      const context = await browser.newContext({ baseURL: base });
      contexts.push(context);
      const joined = await (await context.newPage()).request.post('/api/auth/landing', { data: { token, name } });
      expect(joined.ok()).toBe(true);
    }

    await openProject(owner, id);
    await owner.page.getByRole('button', { name: 'Share', exact: true }).click();
    const share = owner.page.getByRole('dialog', { name: 'Share project' });
    const under = share.getByLabel('Joined through this link');
    await expect(under.getByText('Guest One · guest')).toBeVisible();
    await expect(under.getByText('Guest Two · guest')).toBeVisible();
    await expect(share.getByText('Guest One · guest')).toHaveCount(1);

    // One guest is removed on their own; the other stays under the link.
    await under.getByRole('button', { name: 'Remove Guest One' }).click();
    await expect(under.getByText('Guest One · guest')).toHaveCount(0);
    await expect(under.getByText('Guest Two · guest')).toBeVisible();

    // Revoking the link removes the rest, and the link page explains why it no longer works.
    await share.getByRole('button', { name: 'Revoke' }).click();
    await expect(share.getByText('Guest Two · guest')).toHaveCount(0);
    const context = await browser.newContext({ baseURL: base });
    contexts.push(context);
    const visitor = await context.newPage();
    await visitor.goto(link.path);
    await expect(visitor.getByRole('heading', { name: 'Link unavailable' })).toBeVisible();
    const alert = visitor.getByRole('alert');
    await expect(alert).toHaveText('This link was revoked and can no longer be used. Ask for a new one.');
    await expect(alert).toHaveClass(/\berr\b/);
  });

  test('a guest is offered no Connect AI, since guests cannot create access tokens', async ({ browser }) => {
    const owner = await make(browser, 'Owner');
    const id = await createProject(owner, `Access guest AI ${Date.now()}`);
    const created = await owner.page.request.post(`/api/projects/${id}/shares`, {
      headers: { 'x-csrf-token': owner.csrf }, data: { role: 'editor', expires_days: 7 },
    });
    expect(created.status()).toBe(201);
    const link: { path: string } = await created.json();
    const context = await browser.newContext({ baseURL: base });
    contexts.push(context);
    const guest = await context.newPage();
    const joined = await guest.request.post('/api/auth/landing', { data: { token: link.path.replace('/s/', ''), name: 'AI guest' } });
    expect(joined.ok()).toBe(true);
    const guestUser: { id: string; is_guest: boolean } = (await joined.json()).user;
    expect(guestUser.is_guest).toBe(true);

    // The owner's Share dialog offers Connect AI, so its absence below is meaningful.
    await openProject(owner, id);
    await owner.page.getByRole('button', { name: 'Share', exact: true }).click();
    await expect(owner.page.getByRole('dialog', { name: 'Share project' }).getByRole('button', { name: 'Open Connect AI' })).toBeVisible();

    await guest.goto(`/p/${id}`);
    await dismissTour(guest);
    await expect(guest.getByRole('button', { name: 'Connect AI', exact: true })).toHaveCount(0);

    // The palette does not list the command for a guest, though it lists other commands.
    await guest.getByRole('button', { name: /Search files, commands, sections/ }).click();
    const palette = guest.getByRole('dialog', { name: 'Command palette' });
    await expect(palette.getByRole('option', { name: /Show the welcome tour/ })).toBeVisible();
    await expect(palette.getByRole('option', { name: /Connect an AI client/ })).toHaveCount(0);
    await palette.getByRole('textbox').fill('Connect an AI client');
    await expect(palette.getByText('Nothing matches')).toBeVisible();
    await expect(palette.getByRole('option', { name: /Connect an AI client/ })).toHaveCount(0);
    await palette.getByRole('textbox').press('Escape');
    await expect(palette).toBeHidden();
    await expect(guest.getByRole('dialog', { name: 'Connect an AI client' })).toHaveCount(0);

    // The welcome tour leaves out its Connect AI step for a guest, and counts the steps it shows.
    await guest.getByRole('button', { name: /Search files, commands, sections/ }).click();
    await palette.getByRole('option', { name: /Show the welcome tour/ }).click();
    const tour = guest.getByRole('dialog', { name: 'Welcome tour' });
    await expect(tour).toBeVisible();
    const titles: string[] = [];
    for (let step = 1; ; step++) {
      await expect(tour.locator('.tour-count')).toHaveText(`${step} of 9`);
      titles.push((await tour.locator('h3').textContent()) ?? '');
      if (await tour.getByRole('button', { name: 'Start writing' }).isVisible()) break;
      await tour.getByRole('button', { name: 'Next' }).click();
    }
    expect(titles).toHaveLength(9);
    expect(titles).toContain('Comments and sharing');
    expect(titles).not.toContain('Connect your AI assistant');
    await tour.getByRole('button', { name: 'Start writing' }).click();
    await expect(tour).toBeHidden();

    // A guest made admin reaches the Share dialog, which has no Connect AI there either.
    const raised = await owner.page.request.put(`/api/projects/${id}/members/${guestUser.id}`, {
      headers: { 'x-csrf-token': owner.csrf }, data: { role: 'admin' },
    });
    expect(raised.ok()).toBe(true);
    expect((await raised.json()).status).toBe('applied');
    await guest.getByRole('button', { name: 'Share', exact: true }).click();
    const share = guest.getByRole('dialog', { name: 'Share project' });
    await expect(share).toBeVisible();
    await expect(share.getByRole('button', { name: 'Revoke' }).first()).toBeVisible();
    await expect(share.getByRole('button', { name: 'Open Connect AI' })).toHaveCount(0);
    await expect(share.getByText('AI clients', { exact: true })).toHaveCount(0);
  });

  test('opening a project shows no role notice', async ({ browser }) => {
    const owner = await make(browser, 'Owner');
    const member = await make(browser, 'Viewer');
    const id = await createProject(owner, `Access open ${Date.now()}`);
    await invite(owner, id, member, 'viewer');
    await member.page.goto(`/p/${id}`);
    await expect(member.page.locator('.cm-content')).toBeVisible();
    const notice = toast(member.page, 'Your role is now');
    const until = Date.now() + 2000;
    while (Date.now() < until) {
      expect(await notice.count()).toBe(0);
      await member.page.waitForTimeout(100);
    }
    await expect(member.page.locator('.cm-content')).toHaveAttribute('contenteditable', 'false');
  });
});
