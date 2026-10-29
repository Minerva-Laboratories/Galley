// One Y.Doc per open file, connected to the server with y-websocket and mirrored into IndexedDB
// so editing keeps working while offline. Sessions are cached per project and torn down when the
// project closes.
import * as Y from 'yjs';
import { WebsocketProvider } from 'y-websocket';
import { IndexeddbPersistence } from 'y-indexeddb';
import { wsBase } from '../api';
import { colorFor } from './colors';
import { refreshPresence } from './events';

export const TEXT_NAME = 'content';

export interface DocSession {
  path: string;
  ydoc: Y.Doc;
  ytext: Y.Text;
  provider: WebsocketProvider;
  persistence: IndexeddbPersistence;
  /** Resolves once the document's stored content has loaded, so the editor mounts with it in
   *  place. yCollab does not sync the initial Y.Text into a freshly-created editor. It only
   *  applies later deltas, so mounting before the load races and drops the loaded content. */
  ready: Promise<void>;
  destroy(): void;
}

// Close codes the server uses when it ends a connection for good (SPEC §10.2).
const SIGNED_OUT = 4401;
const ACCESS_REMOVED = 4403;

/** Stops every document of one project from reconnecting once the server has ended this session's
 *  access to it: a document socket closed with 4401 (signed out) or 4403 (access removed). The
 *  documents share that fate, so one such close stops them all, and a document opened later
 *  stays disconnected. Other closes, such as a network drop (1006), reconnect as before. Nothing
 *  is deleted here: offline copies are deleted only by forgetProject, for their owner. */
export class AccessGate {
  private stopped = false;
  private readonly providers = new Set<WebsocketProvider>();

  /** Whether the server has ended access, so no document may connect. */
  get ended(): boolean {
    return this.stopped;
  }

  add(provider: WebsocketProvider) {
    this.providers.add(provider);
    // y-websocket emits `closed` last, after it has handled a close the server meant as final
    // (codes 4400-4499 by default), so stopping here never races its own reconnect logic.
    provider.on('closed', ({ code }) => {
      if (code === SIGNED_OUT || code === ACCESS_REMOVED) this.end();
    });
    if (this.stopped) provider.disconnect();
  }

  remove(provider: WebsocketProvider) {
    this.providers.delete(provider);
  }

  /** Connect a document again, as when an away tab returns, unless access has ended. */
  resume(provider: WebsocketProvider) {
    if (!this.stopped) provider.connect();
  }

  private end() {
    if (this.stopped) return;
    this.stopped = true;
    for (const provider of this.providers) provider.disconnect();
  }
}

const sessions = new Map<string, DocSession>();
let currentProject: string | null = null;
// The current project's documents. A new project, or the same one opened again, gets a new gate.
let gate = new AccessGate();
// An away tab (see away.ts) disconnects every document and reconnects them on return. Edits keep
// landing in IndexedDB either way.
let docsPaused = false;

export function pauseDocs(paused: boolean) {
  docsPaused = paused;
  for (const s of sessions.values()) {
    if (paused) s.provider.disconnect();
    else gate.resume(s.provider);
  }
}

// Which account made a project's offline copies, so signing out deletes only that account's
// copies and never another's in the same browser. Storage is optional: without it nothing is
// recorded, and nothing is deleted on sign-out.
const OWNER_PREFIX = 'galley:idb-owner:';

function recordOwner(projectId: string, userId: string) {
  if (!userId) return;
  try {
    localStorage.setItem(`${OWNER_PREFIX}${projectId}`, userId);
  } catch {
    // Storage is optional.
  }
}

export function openDoc(projectId: string, path: string, userName: string, userId: string): DocSession {
  if (currentProject !== projectId) {
    closeAll();
    currentProject = projectId;
  }
  const existing = sessions.get(path);
  if (existing) return existing;
  recordOwner(projectId, userId);

  const ydoc = new Y.Doc();
  const ytext = ydoc.getText(TEXT_NAME);
  const room = path.split('/').map(encodeURIComponent).join('/');
  const provider = new WebsocketProvider(`${wsBase()}/ws/${encodeURIComponent(projectId)}/doc`, room, ydoc, {
    params: { name: userName },
    maxBackoffTime: 5000,
    connect: !docsPaused && !gate.ended,
  });
  const sessionGate = gate;
  sessionGate.add(provider);
  provider.awareness.setLocalStateField('user', {
    name: userName,
    color: colorFor(userName).color,
    colorLight: colorFor(userName).light,
  });
  const persistence = new IndexeddbPersistence(`galley:${projectId}:${path}`, ydoc);

  // Ready once the locally-persisted content has loaded. Otherwise the load races and drops the
  // content, because yCollab does not backfill a freshly-mounted editor. Server deltas still reach
  // the open editor afterwards. A short cap covers a blocked or absent IndexedDB, as in a private
  // window.
  const ready = Promise.race([
    persistence.whenSynced.then(() => undefined).catch(() => undefined),
    new Promise<void>((resolve) => setTimeout(resolve, 2500)),
  ]);

  const session: DocSession = {
    path,
    ydoc,
    ytext,
    provider,
    persistence,
    ready,
    destroy() {
      sessionGate.remove(provider);
      provider.awareness.setLocalState(null);
      provider.destroy();
      void persistence.destroy();
      ydoc.destroy();
      sessions.delete(path);
    },
  };
  sessions.set(path, session);
  return session;
}

export function setUserName(name: string) {
  refreshPresence();
  for (const s of sessions.values()) {
    const prev = (s.provider.awareness.getLocalState()?.user ?? {}) as Record<string, unknown>;
    s.provider.awareness.setLocalStateField('user', {
      ...prev,
      name,
      color: colorFor(name).color,
      colorLight: colorFor(name).light,
    });
  }
}

/** Tear down one file's session, after a delete or rename. Its offline copy is kept: the server
 *  keeps the file's history too, so a later file of the same name merges cleanly. */
export function closeDoc(path: string) {
  sessions.get(path)?.destroy();
}

export function closeAll() {
  for (const s of Array.from(sessions.values())) s.destroy();
  sessions.clear();
  currentProject = null;
  gate = new AccessGate();
}

/** Resolve when a database is deleted, when deleting it fails or is blocked, or after 3 s, so a
 *  copy that another tab still holds open never hangs the caller. */
function deleteDatabase(name: string): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, 3000);
    const done = () => {
      clearTimeout(timer);
      resolve();
    };
    try {
      const req = indexedDB.deleteDatabase(name);
      req.onsuccess = done;
      req.onerror = done;
      req.onblocked = done;
    } catch {
      done();
    }
  });
}

/** The names of every database in this origin, or null when the browser cannot list them. */
async function databaseNames(): Promise<string[] | null> {
  try {
    if (typeof indexedDB === 'undefined' || typeof indexedDB.databases !== 'function') return null;
    return (await indexedDB.databases()).map((d) => d.name).filter((n): n is string => !!n);
  } catch {
    return null;
  }
}

/** Whether `userId` is the recorded owner of a project's offline copies. A missing key, an empty
 *  id, or unreadable storage all answer no: another account's data is never deleted on a guess. */
function ownsCopies(projectId: string, userId: string | null | undefined): boolean {
  if (!userId) return false;
  try {
    return localStorage.getItem(`${OWNER_PREFIX}${projectId}`) === userId;
  } catch {
    return false;
  }
}

/** Delete this browser's offline copies of a project: after the user lost access to it, or when
 *  it was deleted. `paths` names the files to delete when the browser cannot list databases.
 *  `userId` is the account that is signed in now. The copies are deleted only when that account
 *  is their recorded owner; otherwise they belong to someone else (or to nobody known) and are
 *  kept. The project's open documents in this tab are closed either way. */
export async function forgetProject(projectId: string, paths: string[], userId: string | null | undefined): Promise<void> {
  const prefix = `galley:${projectId}:`;
  // Decided now, before anything else can record a new owner.
  const owned = ownsCopies(projectId, userId);
  // Let a navigation that started this unmount the editor first: a document is never destroyed
  // under a mounted editor, and navigation never waits on storage.
  await new Promise((resolve) => setTimeout(resolve, 0));
  if (currentProject === projectId) {
    for (const s of Array.from(sessions.values())) {
      try {
        if (owned) await s.persistence.clearData();
        else await s.persistence.destroy();
      } catch {
        // The database is deleted below as well, when it is this account's.
      }
      try {
        s.provider.awareness.setLocalState(null);
        s.provider.destroy();
        s.ydoc.destroy();
      } catch {
        // Keep going: the rest still has to be deleted.
      }
      sessions.delete(s.path);
    }
    currentProject = null;
    gate = new AccessGate();
  }
  if (!owned) return;
  try {
    const names = await databaseNames();
    const targets = names ? names.filter((n) => n.startsWith(prefix)) : paths.map((p) => `${prefix}${p}`);
    await Promise.all(targets.map(deleteDatabase));
  } catch {
    // Storage may be unavailable, as in a private window.
  }
  try {
    localStorage.removeItem(`${OWNER_PREFIX}${projectId}`);
  } catch {
    // Storage is optional.
  }
}

/** Delete the offline copies this account made in this browser, on an explicit sign-out. Copies
 *  owned by another account, or with no recorded owner, are left alone. */
export async function forgetUserCopies(userId: string): Promise<void> {
  const projects: string[] = [];
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key?.startsWith(OWNER_PREFIX) && localStorage.getItem(key) === userId) projects.push(key.slice(OWNER_PREFIX.length));
    }
  } catch {
    return;
  }
  for (const projectId of projects) await forgetProject(projectId, [], userId);
}
