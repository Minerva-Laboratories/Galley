import { expect, test, type WebSocketRoute } from '@playwright/test';
import { authenticate, dismissTour } from './auth';

test('presence follows people across files, duplicate tabs and event reconnection', async ({ browser }) => {
  const ownerContext = await browser.newContext();
  const guestContext = await browser.newContext();
  try {
    const owner = await ownerContext.newPage();
    await authenticate(owner);
    const csrf = (await ownerContext.cookies()).find((cookie) => cookie.name === 'galley_csrf')!.value;
    const headers = { 'x-csrf-token': csrf };
    const created = await owner.request.post('/api/projects', { headers, data: { name: `Presence ${Date.now()}` } });
    expect(created.ok()).toBeTruthy();
    const { id } = await created.json();
    const me = (await (await owner.request.get('/api/auth/me')).json()).user;
    const shared = await owner.request.post(`/api/projects/${id}/shares`, { headers, data: { role: 'viewer', label: 'Presence test' } });
    expect(shared.ok()).toBeTruthy();
    const { path } = await shared.json();
    const guest = await guestContext.newPage();
    // A separate authenticated guest deliberately has the owner's display name.
    const landed = await guest.request.post('/api/auth/landing', { data: { token: path.slice(3), name: me.name } });
    expect(landed.ok()).toBeTruthy();
    let eventSocket: { client: WebSocketRoute; server: WebSocketRoute } | undefined;
    let connections = 0;
    await guest.routeWebSocket(`**/ws/${id}/events`, (socket) => {
      eventSocket = { client: socket, server: socket.connectToServer() };
      connections++;
    });
    await owner.goto(`/p/${id}`);
    await dismissTour(owner);
    await guest.goto(`/p/${id}`);
    await dismissTour(guest);
    await expect(owner.locator('.presence .n')).toHaveText('2 online');
    await expect(guest.locator('.presence .n')).toHaveText('2 online');
    await guest.getByTitle('Open refs.bib', { exact: true }).click();
    await expect(guest.getByRole('tab', { name: 'refs.bib' })).toHaveAttribute('aria-selected', 'true');
    await expect(guest.locator('.presence .n')).toHaveText('2 online');
    await expect(owner.locator('.presence .n')).toHaveText('2 online');

    const duplicate = await guestContext.newPage();
    await duplicate.goto(`/p/${id}`);
    await expect(duplicate.locator('.presence .n')).toHaveText('2 online');
    await expect(owner.locator('.presence .n')).toHaveText('2 online');
    await duplicate.close();
    await expect(owner.locator('.presence .n')).toHaveText('2 online');
    // Drop only the event socket: the document remains open on refs.bib. The next socket must
    // receive a full project roster without relying on document awareness changes.
    const before = connections;
    await Promise.all([
      eventSocket!.client.close({ code: 1012, reason: 'Presence reconnection test' }),
      eventSocket!.server.close({ code: 1012, reason: 'Presence reconnection test' }),
    ]);
    await expect.poll(() => connections).toBeGreaterThan(before);
    await expect(guest.locator('.presence .n')).toHaveText('2 online');
    await expect(owner.locator('.presence .n')).toHaveText('2 online');
    await guestContext.close();
    await expect(owner.locator('.presence .n')).toHaveText('Only you');
  } finally {
    await ownerContext.close();
    await guestContext.close();
  }
});
