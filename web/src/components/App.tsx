import { useEffect, useLayoutEffect } from 'preact/hooks';
import { loadMe } from '../store/auth';
import { accentColor, authReady, currentUser, route, theme } from '../store/store';
import { AuthScreen, LandingScreen } from './Auth';
import { EditorPage } from './EditorPage';
import { ProjectsPage } from './ProjectsPage';
import { accentTokens, DEFAULT_ACCENT, normalizeHex } from '../util/accent';
import { Toast } from './Toast';

export function App() {
  useLayoutEffect(() => {
    const root = document.documentElement;
    root.dataset.theme = theme.value;
    for (const [name, value] of Object.entries(accentTokens(accentColor.value, theme.value))) root.style.setProperty(name, value);
    document.querySelector('meta[name="theme-color"]')?.setAttribute('content', accentColor.value);
  }, [theme.value, accentColor.value]);

  useEffect(() => {
    const sync = (event: StorageEvent) => {
      if (event.storageArea !== localStorage) return;
      if (event.key === 'galley.accent' || event.key === null) accentColor.value = normalizeHex(event.newValue ?? '') ?? DEFAULT_ACCENT;
      if (event.key === 'galley.theme' && (event.newValue === 'light' || event.newValue === 'dark')) theme.value = event.newValue;
    };
    window.addEventListener('storage', sync);
    return () => window.removeEventListener('storage', sync);
  }, []);

  useEffect(() => {
    void loadMe();
  }, []);

  const r = route.value;

  if (!authReady.value) {
    return <div class="center">Loading…</div>;
  }
  // A share link is reachable whether or not you are signed in.
  if (r.kind === 'landing') {
    return (
      <>
        <LandingScreen token={r.token} />
        <Toast />
      </>
    );
  }
  if (!currentUser.value) {
    return (
      <>
        <AuthScreen />
        <Toast />
      </>
    );
  }
  return (
    <>
      {r.kind === 'editor' ? <EditorPage key={r.id} id={r.id} /> : <ProjectsPage />}
      <Toast />
    </>
  );
}
