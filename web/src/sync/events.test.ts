import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '../api';
import { connectionStatus, pauseEvents, refreshPresence, subscribeEvents } from './events';

const getProject = vi.hoisted(() => vi.fn());
vi.mock('../api', () => ({
  wsBase: () => 'ws://galley.test',
  api: { getProject },
  ApiError: class ApiError extends Error {
    constructor(readonly status: number, message = 'failed') { super(message); }
  },
}));

class Socket {
  static OPEN = 1;
  static all: Socket[] = [];
  readyState = 0;
  onopen?: () => void;
  onmessage?: (event: { data: string }) => void;
  onclose?: (event: { code: number }) => void;
  onerror?: () => void;
  send = vi.fn();
  constructor(readonly url: string) { Socket.all.push(this); }
  open() { this.readyState = 1; this.onopen?.(); }
  close(code = 1006) { this.readyState = 3; this.onclose?.({ code }); }
  message(data: unknown) { this.onmessage?.({ data: JSON.stringify(data) }); }
}

describe('project event connections', () => {
  let stop: (() => void) | undefined;
  beforeEach(() => {
    vi.useFakeTimers();
    Socket.all = [];
    vi.stubGlobal('WebSocket', Socket);
    vi.stubGlobal('window', new EventTarget());
  });
  afterEach(() => {
    stop?.();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('reconnects with a fresh full roster and ignores frames from the old socket', () => {
    const event = vi.fn();
    const status = vi.fn();
    stop = subscribeEvents('a project', event, status);
    const first = Socket.all[0]!;
    expect(first.url).toBe('ws://galley.test/ws/a%20project/events');
    first.open();
    first.message({ type: 'presence', users: [{ id: 'ana', name: 'Ana' }] });
    first.close();
    expect(status).toHaveBeenLastCalledWith('offline');
    vi.advanceTimersByTime(1000);
    const second = Socket.all[1]!;
    second.open();
    second.message({ type: 'presence', users: [{ id: 'bob', name: 'Bob' }] });
    first.message({ type: 'presence', users: [] });
    expect(event).toHaveBeenCalledTimes(2);
    expect(event).toHaveBeenLastCalledWith({ type: 'presence', users: [{ id: 'bob', name: 'Bob' }] });
    expect(status).toHaveBeenLastCalledWith('online');
  });

  it('retries immediately when the network returns and cancels pending retries on disposal', () => {
    stop = subscribeEvents('paper', vi.fn());
    Socket.all[0]!.close();
    window.dispatchEvent(new Event('online'));
    expect(Socket.all).toHaveLength(2);
    Socket.all[1]!.open();
    refreshPresence();
    expect(Socket.all[1]!.send).toHaveBeenCalledWith('{"type":"presence_refresh"}');
    vi.advanceTimersByTime(20_000);
    expect(Socket.all).toHaveLength(2);
    Socket.all[1]!.close();
    stop();
    vi.advanceTimersByTime(20_000);
    window.dispatchEvent(new Event('online'));
    expect(Socket.all).toHaveLength(2);
    refreshPresence();
    expect(Socket.all[1]!.send).toHaveBeenCalledTimes(1);
  });
});

// The edit transport determines whether edits are local; project events must also be connected
// before the whole workspace is online.
describe('workspace connection status', () => {
  it('requires both active transports to be online', () => {
    expect(connectionStatus('online', 'online')).toBe('online');
    expect(connectionStatus('offline', 'online')).toBe('connecting');
    expect(connectionStatus('connecting', 'online')).toBe('connecting');
    expect(connectionStatus('online', 'connecting')).toBe('connecting');
  });
  it('shows offline edits for a disconnected document and follows project events without a document', () => {
    expect(connectionStatus('online', 'offline')).toBe('offline');
    expect(connectionStatus('offline', 'offline')).toBe('offline');
    expect(connectionStatus('online', null)).toBe('online');
    expect(connectionStatus('connecting', null)).toBe('connecting');
    expect(connectionStatus('offline', null)).toBe('offline');
  });
});

describe('away tabs and the event socket', () => {
  let stop: (() => void) | undefined;
  beforeEach(() => {
    vi.useFakeTimers();
    Socket.all = [];
    vi.stubGlobal('WebSocket', Socket);
    vi.stubGlobal('window', new EventTarget());
  });
  afterEach(() => {
    stop?.();
    pauseEvents(false);
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('an away tab closes its socket without retrying and reconnects when it comes back', () => {
    const statuses: string[] = [];
    stop = subscribeEvents('p', () => {}, (s) => statuses.push(s));
    Socket.all[0]!.open();
    pauseEvents(true);
    expect(Socket.all[0]!.readyState).toBe(3);
    vi.advanceTimersByTime(60_000);
    expect(Socket.all).toHaveLength(1);
    expect(statuses.at(-1)).toBe('offline');
    pauseEvents(false);
    expect(Socket.all).toHaveLength(2);
    Socket.all[1]!.open();
    expect(statuses.at(-1)).toBe('online');
  });
});

describe('lost access on the event socket', () => {
  let stop: (() => void) | undefined;
  beforeEach(() => {
    vi.useFakeTimers();
    Socket.all = [];
    getProject.mockReset();
    vi.stubGlobal('WebSocket', Socket);
    vi.stubGlobal('window', new EventTarget());
  });
  afterEach(() => {
    stop?.();
    pauseEvents(false);
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  // Nothing may open a new socket: not a retry, an away tab returning, or the network coming back.
  const expectNoReconnect = () => {
    const count = Socket.all.length;
    pauseEvents(true);
    pauseEvents(false);
    window.dispatchEvent(new Event('online'));
    vi.advanceTimersByTime(60_000);
    expect(Socket.all).toHaveLength(count);
  };

  it.each(['access_removed', 'signed_out'])('stops for good after a %s frame', (type) => {
    const event = vi.fn();
    stop = subscribeEvents('p', event);
    Socket.all[0]!.open();
    Socket.all[0]!.message({ type });
    expect(event).toHaveBeenCalledWith({ type });
    Socket.all[0]!.close(type === 'access_removed' ? 4403 : 4401);
    expectNoReconnect();
  });

  it.each([4403, 4401])('stops for good after close %i', (code) => {
    const status = vi.fn();
    stop = subscribeEvents('p', vi.fn(), status);
    Socket.all[0]!.open();
    Socket.all[0]!.close(code);
    expect(status).toHaveBeenLastCalledWith('offline');
    expectNoReconnect();
    expect(getProject).not.toHaveBeenCalled();
  });

  it.each([
    [4403, 'access_removed'],
    [4401, 'signed_out'],
  ])('close %i with no notice before it delivers %s once', (code, type) => {
    const event = vi.fn();
    stop = subscribeEvents('p', event);
    Socket.all[0]!.open();
    Socket.all[0]!.close(code);
    expect(event).toHaveBeenCalledTimes(1);
    expect(event).toHaveBeenCalledWith({ type });
    expectNoReconnect();
    expect(event).toHaveBeenCalledTimes(1);
  });

  it.each([
    [4403, 'access_removed'],
    [4401, 'signed_out'],
  ])('a notice followed by close %i delivers %s only once', (code, type) => {
    const event = vi.fn();
    stop = subscribeEvents('p', event);
    Socket.all[0]!.open();
    Socket.all[0]!.message({ type });
    Socket.all[0]!.close(code);
    expect(event).toHaveBeenCalledTimes(1);
    expect(event).toHaveBeenCalledWith({ type });
    expectNoReconnect();
    expect(event).toHaveBeenCalledTimes(1);
  });

  const twoFailedConnects = async () => {
    Socket.all[0]!.close();
    expect(getProject).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1000);
    Socket.all[1]!.close();
    await vi.advanceTimersByTimeAsync(0);
  };

  it.each([
    [404, 'access_removed'],
    [401, 'signed_out'],
  ])('asks the server after two failed connects, and %i reports %s', async (status, type) => {
    getProject.mockRejectedValue(new ApiError(status, 'no'));
    const event = vi.fn();
    stop = subscribeEvents('p', event);
    await twoFailedConnects();
    expect(getProject).toHaveBeenCalledTimes(1);
    expect(getProject).toHaveBeenCalledWith('p');
    expect(event).toHaveBeenCalledWith({ type });
    expectNoReconnect();
  });

  it('keeps retrying when the check fails for another reason', async () => {
    getProject.mockRejectedValue(new ApiError(500, 'down'));
    const event = vi.fn();
    stop = subscribeEvents('p', event);
    await twoFailedConnects();
    expect(getProject).toHaveBeenCalledTimes(1);
    expect(event).not.toHaveBeenCalled();
    vi.advanceTimersByTime(2000);
    expect(Socket.all).toHaveLength(3);
  });

  it('an open socket resets the count of failed connects', async () => {
    stop = subscribeEvents('p', vi.fn());
    Socket.all[0]!.close();
    vi.advanceTimersByTime(1000);
    Socket.all[1]!.open();
    Socket.all[1]!.close();
    vi.advanceTimersByTime(1000);
    Socket.all[2]!.close();
    await vi.advanceTimersByTimeAsync(0);
    expect(getProject).not.toHaveBeenCalled();
  });
});
