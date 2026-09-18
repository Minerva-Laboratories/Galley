import { useEffect, useRef, useState } from 'preact/hooks';
import { api } from '../api';
import { login, messageOf, signup } from '../store/auth';
import { currentUser, navigate, needsSetup, publicSignup, setDisplayName, theme, toggleTheme } from '../store/store';
import { Icon } from './Icon';

function Shell({ children }: { children: preact.ComponentChildren }) {
  return (
    <div class="auth">
      <button
        class="tb icon auth-theme"
        title={theme.value === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
        aria-label="Toggle theme"
        onClick={toggleTheme}
      >
        <Icon name="theme" size={16} />
      </button>
      <div class="auth-card">
        <div class="wordmark" aria-label="Galley">
          galley<span class="caret">^</span>
        </div>
        {children}
      </div>
    </div>
  );
}

/** Sign in, first-run admin setup, or open signup. The server state chooses. */
export function AuthScreen() {
  const setup = needsSetup.value;
  const [mode, setMode] = useState<'login' | 'signup'>(setup || publicSignup.value ? 'signup' : 'login');
  const [email, setEmail] = useState('');
  const [name, setName] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const first = useRef<HTMLInputElement>(null);
  useEffect(() => first.current?.focus(), [mode]);

  const submit = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if (mode === 'signup') await signup(email, name, password);
      else await login(email, password);
    } catch (err) {
      setError(messageOf(err, 'Something went wrong. Try again.'));
      setBusy(false);
    }
  };

  return (
    <Shell>
      <h1 class="auth-h">{setup ? 'Create your admin account' : mode === 'signup' ? 'Create an account' : 'Sign in'}</h1>
      {setup && <p class="auth-sub">This is the first account on this server, so it becomes the admin.</p>}
      <form onSubmit={submit}>
        {mode === 'signup' && (
          <input ref={mode === 'signup' ? first : undefined} class="auth-in" placeholder="Your name" value={name} onInput={(e) => setName((e.target as HTMLInputElement).value)} autocomplete="name" />
        )}
        <input ref={mode === 'login' ? first : undefined} class="auth-in" type="email" placeholder="name@university.edu" value={email} onInput={(e) => setEmail((e.target as HTMLInputElement).value)} autocomplete="email" />
        <input class="auth-in" type="password" placeholder="Password" value={password} onInput={(e) => setPassword((e.target as HTMLInputElement).value)} autocomplete={mode === 'signup' ? 'new-password' : 'current-password'} />
        {mode === 'signup' && <div class="hint" style={{ padding: '0 0 8px' }}>At least 8 characters.</div>}
        {error && <div class="err">{error}</div>}
        <button class="tb primary auth-submit" type="submit" disabled={busy || !email || !password || (mode === 'signup' && !name)}>
          {busy ? 'One moment…' : setup ? 'Create admin account' : mode === 'signup' ? 'Create account' : 'Sign in'}
        </button>
      </form>
      {!setup && publicSignup.value && (
        <button class="auth-switch" onClick={() => setMode(mode === 'login' ? 'signup' : 'login')}>
          {mode === 'login' ? 'No account? Create one' : 'Have an account? Sign in'}
        </button>
      )}
      {!setup && !publicSignup.value && mode === 'login' && (
        <p class="auth-sub" style={{ marginTop: 14 }}>New accounts are added by an admin, or through a share link.</p>
      )}
    </Shell>
  );
}

/** Share-link landing: name yourself, then join. */
export function LandingScreen({ token }: { token: string }) {
  const [preview, setPreview] = useState<{ project_name: string; role: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    api
      .sharePreview(token)
      .then((p) => setPreview(p))
      .catch((e) => setError(messageOf(e, 'This share link is invalid or expired.')));
  }, [token]);
  useEffect(() => input.current?.focus(), [preview]);

  const join = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const res = await api.landing(token, name);
      currentUser.value = res.user;
      setDisplayName(res.user.name);
      // The server names the project. Fall back to reloading the projects page.
      const project = (res as { project_id?: string }).project_id;
      navigate(project ? `/p/${project}` : '/');
    } catch (err) {
      setError(messageOf(err, 'Could not join. The link may have expired.'));
      setBusy(false);
    }
  };

  return (
    <Shell>
      {error && !preview ? (
        <>
          <h1 class="auth-h">Link unavailable</h1>
          <p class="auth-sub">{error}</p>
          <button class="tb" style={{ margin: '10px auto 0' }} onClick={() => navigate('/')}>
            Go to Galley
          </button>
        </>
      ) : (
        <>
          <h1 class="auth-h">Join {preview?.project_name ?? '…'}</h1>
          <p class="auth-sub">You're invited to {preview ? roleVerb(preview.role) : 'view'}. Choose a name so collaborators know who you are.</p>
          <form onSubmit={join}>
            <input ref={input} class="auth-in" placeholder="Your name" value={name} maxLength={40} onInput={(e) => setName((e.target as HTMLInputElement).value)} />
            {error && <div class="err">{error}</div>}
            <button class="tb primary auth-submit" type="submit" disabled={busy || !name.trim() || !preview}>
              {busy ? 'Joining…' : 'Join project'}
            </button>
          </form>
        </>
      )}
    </Shell>
  );
}

function roleVerb(role: string): string {
  switch (role) {
    case 'editor':
      return 'edit this project';
    case 'commenter':
      return 'comment and suggest';
    default:
      return 'view this project';
  }
}
