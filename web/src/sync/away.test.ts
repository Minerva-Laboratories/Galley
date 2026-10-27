import { describe, expect, it } from 'vitest';
import { HIDDEN_MS, INACTIVE_MS, watchAway, type AwayEnv } from './away';

function fakeTab() {
  let now = 0;
  let tick: (() => void) | undefined;
  const doc = Object.assign(new EventTarget(), { visibilityState: 'visible' as DocumentVisibilityState });
  const win = Object.assign(new EventTarget(), {
    setInterval: (fn: () => void) => { tick = fn; return 1; },
    clearInterval: () => { tick = undefined; },
  });
  const env = { doc, win, now: () => now } as unknown as AwayEnv;
  return {
    env,
    advance(ms: number) { now += ms; tick?.(); },
    hide() { doc.visibilityState = 'hidden'; doc.dispatchEvent(new Event('visibilitychange')); },
    show() { doc.visibilityState = 'visible'; doc.dispatchEvent(new Event('visibilitychange')); },
    type() { win.dispatchEvent(new Event('keydown')); },
    stopped: () => tick === undefined,
  };
}

describe('away tabs', () => {
  it('a tab hidden for ten minutes is away, and showing it brings it back at once', () => {
    const tab = fakeTab();
    const seen: boolean[] = [];
    watchAway((a) => seen.push(a), tab.env);
    tab.hide();
    tab.advance(HIDDEN_MS - 1000);
    expect(seen).toEqual([]);
    tab.advance(1000);
    expect(seen).toEqual([true]);
    tab.show();
    expect(seen).toEqual([true, false]);
  });

  it('a visible tab is away only after thirty minutes without input', () => {
    const tab = fakeTab();
    const seen: boolean[] = [];
    watchAway((a) => seen.push(a), tab.env);
    tab.advance(INACTIVE_MS - 60_000);
    tab.type();
    tab.advance(INACTIVE_MS - 60_000);
    expect(seen, 'typing resets the clock').toEqual([]);
    tab.advance(60_000);
    expect(seen).toEqual([true]);
    tab.type();
    expect(seen).toEqual([true, false]);
  });

  it('a short trip to another tab changes nothing', () => {
    const tab = fakeTab();
    const seen: boolean[] = [];
    const stop = watchAway((a) => seen.push(a), tab.env);
    tab.hide();
    tab.advance(5 * 60_000);
    tab.show();
    tab.advance(5 * 60_000);
    expect(seen).toEqual([]);
    stop();
    expect(tab.stopped()).toBe(true);
  });
});
