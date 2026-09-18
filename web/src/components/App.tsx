import { useEffect } from 'preact/hooks';
import { loadMe } from '../store/auth';
import { authReady, currentUser, route, theme } from '../store/store';
import { AuthScreen, LandingScreen } from './Auth';
import { EditorPage } from './EditorPage';
import { ProjectsPage } from './ProjectsPage';
import { Toast } from './Toast';

export function App() {
  useEffect(() => {
    document.documentElement.dataset.theme = theme.value;
  }, [theme.value]);

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
