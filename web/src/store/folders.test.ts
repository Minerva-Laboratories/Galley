import { beforeAll, describe, expect, it, vi } from 'vitest';

// The store reads the address bar and the window when it loads, so they exist before it does.
let store: typeof import('./store');
beforeAll(async () => {
  vi.stubGlobal('location', new URL('http://galley.test/'));
  vi.stubGlobal('window', Object.assign(new EventTarget(), { matchMedia: () => ({ matches: false, addEventListener() {} }) }));
  vi.stubGlobal('document', { documentElement: { dataset: {}, style: { setProperty() {} } } });
  store = await import('./store');
});

describe('the folder list', () => {
  it('a reply that started earlier never overwrites a newer one', () => {
    const slow = store.folderTicket();
    const fast = store.folderTicket();
    store.applyFolders(fast, ['figures', 'figures/plots']);
    store.applyFolders(slow, ['figures']);
    expect(store.folders.value).toEqual(['figures', 'figures/plots']);
  });

  it('a local change retires fetches already in flight', () => {
    const inFlight = store.folderTicket();
    store.applyFolders(store.folderTicket(), ['notes']);
    store.applyFolders(inFlight, ['notes', 'deleted']);
    expect(store.folders.value).toEqual(['notes']);
  });
});
