// Project events and presence share one authenticated socket, independent of the active file.
import { wsBase, type ProjectEvent } from '../api';

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

export function subscribeEvents(
  projectId: string,
  onEvent: (e: ProjectEvent) => void,
  onStatus: (status: EventConnection) => void = () => {},
): () => void {
  let socket: WebSocket | null = null;
  let closed = false;
  let retry = 1000;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const connect = () => {
    if (closed || eventsPaused) return;
    clearTimeout(timer);
    onStatus('connecting');
    const current = new WebSocket(`${wsBase()}/ws/${encodeURIComponent(projectId)}/events`);
    socket = current;
    current.onopen = () => {
      if (closed || socket !== current) return;
      retry = 1000;
      onStatus('online');
    };
    current.onmessage = (m) => {
      if (closed || socket !== current) return;
      try {
        onEvent(JSON.parse(String(m.data)) as ProjectEvent);
      } catch {
        // Ignore malformed frames. The next one is independent.
      }
    };
    current.onclose = () => {
      if (closed || socket !== current) return;
      onStatus('offline');
      timer = setTimeout(connect, retry);
      retry = Math.min(retry * 2, 10000);
    };
    // A failed connection will close too; explicitly closing also covers implementations that
    // report an error without promptly finishing their connection attempt.
    current.onerror = () => { if (socket === current) current.close(); };
  };
  const reconnect = () => {
    if (closed || eventsPaused) return;
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
