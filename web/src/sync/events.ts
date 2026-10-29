// Project events and presence share one authenticated socket, independent of the active file.
import { api, ApiError, wsBase, type ProjectEvent } from '../api';

export type EventConnection = 'online' | 'connecting' | 'offline';

/** An offline document stores edits locally. A live document with a lost event socket keeps
 * syncing edits, but the project is reconnecting until presence and project events return. */
export function connectionStatus(project: EventConnection, document: EventConnection | null): EventConnection {
  if (document === null || document === 'offline') return document ?? project;
  return project === 'online' && document === 'online' ? 'online' : 'connecting';
}

let refreshCurrentPresence: (() => void) | undefined;
export function refreshPresence() { refreshCurrentPresence?.(); }

// An away tab (see away.ts) closes its event socket and opens it again on return.
let eventsPaused = false;
let pauseCurrent: ((paused: boolean) => void) | undefined;
export function pauseEvents(paused: boolean) {
  eventsPaused = paused;
  pauseCurrent?.(paused);
}

// Close codes the server uses when it ends a connection for good (SPEC §10.2).
const SIGNED_OUT = 4401;
const ACCESS_REMOVED = 4403;

export function subscribeEvents(
  projectId: string,
  onEvent: (e: ProjectEvent) => void,
  onStatus: (status: EventConnection) => void = () => {},
): () => void {
  let socket: WebSocket | null = null;
  let closed = false;
  let retry = 1000;
  let timer: ReturnType<typeof setTimeout> | undefined;
  // Set once access is gone or the session ended. Nothing reconnects after that: not a retry, not
  // the network coming back, not an away tab returning.
  let stopped = false;
  // Closes in a row before the socket opened. A refused upgrade looks like a network failure, so
  // from the second one on, the project is fetched to tell a removal from an outage.
  let failures = 0;
  let checking = false;
  // Set once access_removed or signed_out has reached the handler, so a close code that follows
  // the JSON notice does not deliver it again.
  let noticed = false;

  const stop = () => {
    stopped = true;
    clearTimeout(timer);
  };
  const deliver = (event: ProjectEvent) => {
    try {
      if (event.type === 'access_removed' || event.type === 'signed_out') {
        if (noticed) return;
        noticed = true;
      }
      onEvent(event);
    } catch {
      // A failing handler must not break the connection logic.
    }
  };
  const checkAccess = () => {
    if (checking) return;
    checking = true;
    api.getProject(projectId).then(
      () => { checking = false; },
      (e: unknown) => {
        checking = false;
        if (closed || stopped || !(e instanceof ApiError)) return;
        if (e.status === 404 || e.status === 401) {
          stop();
          const previous = socket;
          socket = null;
          previous?.close();
          onStatus('offline');
          deliver(e.status === 404 ? { type: 'access_removed' } : { type: 'signed_out' });
        }
      },
    );
  };

  const connect = () => {
    if (closed || stopped || eventsPaused) return;
    clearTimeout(timer);
    onStatus('connecting');
    const current = new WebSocket(`${wsBase()}/ws/${encodeURIComponent(projectId)}/events`);
    socket = current;
    let opened = false;
    current.onopen = () => {
      if (closed || socket !== current) return;
      opened = true;
      retry = 1000;
      failures = 0;
      onStatus('online');
    };
    current.onmessage = (m) => {
      if (closed || socket !== current) return;
      let event: ProjectEvent;
      try {
        event = JSON.parse(String(m.data)) as ProjectEvent;
      } catch {
        // Ignore malformed frames. The next one is independent.
        return;
      }
      if (event.type === 'access_removed' || event.type === 'signed_out') stop();
      deliver(event);
    };
    current.onclose = (e?: { code?: number }) => {
      if (closed || socket !== current) return;
      onStatus('offline');
      const code = e?.code;
      if (code === SIGNED_OUT || code === ACCESS_REMOVED) {
        stop();
        // A close code without the JSON notice before it still has to reach the page, or it
        // would sit there silently with a dead socket.
        deliver(code === ACCESS_REMOVED ? { type: 'access_removed' } : { type: 'signed_out' });
      }
      if (stopped) return;
      if (!opened) failures += 1;
      if (failures >= 2) checkAccess();
      timer = setTimeout(connect, retry);
      retry = Math.min(retry * 2, 10000);
    };
    // A failed connection will close too; explicitly closing also covers implementations that
    // report an error without promptly finishing their connection attempt.
    current.onerror = () => { if (socket === current) current.close(); };
  };
  const reconnect = () => {
    if (closed || stopped || eventsPaused) return;
    const previous = socket;
    socket = null; // Ignore the old close callback and its reconnect timer.
    previous?.close();
    connect();
  };
  const refresh = () => {
    if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: 'presence_refresh' }));
  };
  const pause = (paused: boolean) => {
    if (closed) return;
    if (!paused) return reconnect();
    clearTimeout(timer);
    const previous = socket;
    socket = null;
    previous?.close();
    onStatus('offline');
  };
  refreshCurrentPresence = refresh;
  pauseCurrent = pause;
  window.addEventListener('online', reconnect);
  if (eventsPaused) onStatus('offline');
  connect();

  return () => {
    closed = true;
    clearTimeout(timer);
    window.removeEventListener('online', reconnect);
    if (refreshCurrentPresence === refresh) refreshCurrentPresence = undefined;
    if (pauseCurrent === pause) pauseCurrent = undefined;
    socket?.close();
  };
}
