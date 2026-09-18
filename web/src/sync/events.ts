// Project-level events (commits, new files) pushed by the server over one JSON WebSocket.
import { wsBase, type ProjectEvent } from '../api';

export function subscribeEvents(projectId: string, onEvent: (e: ProjectEvent) => void): () => void {
  let socket: WebSocket | null = null;
  let closed = false;
  let retry = 1000;

  const connect = () => {
    if (closed) return;
    socket = new WebSocket(`${wsBase()}/ws/${encodeURIComponent(projectId)}/events`);
    socket.onopen = () => (retry = 1000);
    socket.onmessage = (m) => {
      try {
        onEvent(JSON.parse(String(m.data)) as ProjectEvent);
      } catch {
        // Ignore malformed frames. The next one is independent.
      }
    };
    socket.onclose = () => {
      if (closed) return;
      setTimeout(connect, retry);
      retry = Math.min(retry * 2, 10000);
    };
  };
  connect();

  return () => {
    closed = true;
    socket?.close();
  };
}
