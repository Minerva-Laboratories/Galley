// A connected tab keeps the server awake, and a host that stops idle servers bills every hour one
// forgotten tab holds it. A tab that has been hidden for 10 minutes, or visible with no input for
// 30, counts as away and lets go of its connections. Coming back reconnects at once. Edits never
// wait on this: they live in IndexedDB and sync on reconnect.

export const HIDDEN_MS = 10 * 60_000;
export const INACTIVE_MS = 30 * 60_000;
const CHECK_MS = 30_000;
const ACTIVITY = ['pointerdown', 'pointermove', 'keydown', 'wheel', 'touchstart', 'scroll', 'focus'] as const;

export interface AwayEnv {
  doc: Pick<Document, 'visibilityState' | 'addEventListener' | 'removeEventListener'>;
  win: Pick<Window, 'addEventListener' | 'removeEventListener' | 'setInterval' | 'clearInterval'>;
  now: () => number;
}

const browser = (): AwayEnv => ({ doc: document, win: window, now: Date.now });

/** Call `onChange` when the tab becomes away or comes back. Returns a function that stops. */
export function watchAway(onChange: (away: boolean) => void, env: AwayEnv = browser()): () => void {
  let away = false;
  let lastInput = env.now();
  let hiddenSince = env.doc.visibilityState === 'hidden' ? env.now() : null;

  const set = (next: boolean) => {
    if (next === away) return;
    away = next;
    onChange(away);
  };
  const check = () => {
    const now = env.now();
    set(hiddenSince !== null ? now - hiddenSince >= HIDDEN_MS : now - lastInput >= INACTIVE_MS);
  };
  const active = () => {
    lastInput = env.now();
    if (env.doc.visibilityState !== 'hidden') set(false);
  };
  const visibility = () => {
    if (env.doc.visibilityState === 'hidden') {
      hiddenSince = env.now();
    } else {
      hiddenSince = null;
      active();
    }
  };

  env.doc.addEventListener('visibilitychange', visibility);
  for (const name of ACTIVITY) env.win.addEventListener(name, active, { capture: true, passive: true });
  const timer = env.win.setInterval(check, CHECK_MS);
  return () => {
    env.win.clearInterval(timer);
    env.doc.removeEventListener('visibilitychange', visibility);
    for (const name of ACTIVITY) env.win.removeEventListener(name, active, { capture: true });
  };
}
