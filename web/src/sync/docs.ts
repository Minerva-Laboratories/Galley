// One Y.Doc per open file, connected to the server with y-websocket and mirrored into IndexedDB
// so editing keeps working while offline. Sessions are cached per project and torn down when the
// project closes.
import * as Y from 'yjs';
import { WebsocketProvider } from 'y-websocket';
import { IndexeddbPersistence } from 'y-indexeddb';
import { wsBase } from '../api';
import { colorFor } from './colors';

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

const sessions = new Map<string, DocSession>();
let currentProject: string | null = null;

export function openDoc(projectId: string, path: string, userName: string): DocSession {
  if (currentProject !== projectId) {
    closeAll();
    currentProject = projectId;
  }
  const existing = sessions.get(path);
  if (existing) return existing;

  const ydoc = new Y.Doc();
  const ytext = ydoc.getText(TEXT_NAME);
  const room = path.split('/').map(encodeURIComponent).join('/');
  const provider = new WebsocketProvider(`${wsBase()}/ws/${encodeURIComponent(projectId)}/doc`, room, ydoc, {
    params: { name: userName },
    maxBackoffTime: 5000,
  });
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
}
