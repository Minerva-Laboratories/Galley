import { useEffect, useRef, useState } from 'preact/hooks';
import { api, ApiError, type ProjectMeta, type Template } from '../api';
import { ago } from '../util/time';
import { logout } from '../store/auth';
import { currentUser, navigate, theme, toggleTheme } from '../store/store';
import { Icon } from './Icon';

export function ProjectsPage() {
  const [projects, setProjects] = useState<ProjectMeta[] | null>(null);
  const [templates, setTemplates] = useState<Template[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [picked, setPicked] = useState('blank-article');
  const [name, setName] = useState('');
  const [query, setQuery] = useState('');
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    document.title = 'Projects — Galley';
    api
      .listProjects()
      .then(setProjects)
      .catch((e) => setError(e instanceof ApiError ? e.message : 'Could not load projects.'));
    api.listTemplates().then(setTemplates).catch(() => setTemplates([]));
  }, []);

  useEffect(() => {
    if (creating) input.current?.focus();
  }, [creating]);

  const create = async () => {
    const n = name.trim();
    if (!n) return;
    try {
      const meta = await api.createProject(n, picked);
      navigate(`/p/${meta.id}`);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not create the project.');
    }
  };

  const start = (template: string) => {
    setPicked(template);
    setCreating(true);
  };

  const nameForm = (
    <>
      <input
        ref={input}
        placeholder="Project name"
        value={name}
        maxLength={120}
        style={{ fontFamily: 'var(--font)' }}
        onInput={(e) => setName((e.target as HTMLInputElement).value)}
        onKeyDown={(e) => e.key === 'Escape' && setCreating(false)}
        aria-label="Project name"
      />
      <button class="tb primary" type="submit" disabled={!name.trim()}>
        Create
      </button>
    </>
  );

  const shown = (projects ?? []).filter((p) => p.name.toLowerCase().includes(query.trim().toLowerCase()));
  const empty = projects !== null && projects.length === 0;

  return (
    <div class="projects">
      <header class="top">
        <span class="wordmark">
          galley<span class="caret">^</span>
        </span>
        <span style={{ color: 'var(--text-3)' }}>Write together. Compile anywhere.</span>
        <span style={{ marginLeft: 'auto' }} />
        {currentUser.value && <span style={{ color: 'var(--text-2)', fontSize: 12 }}>{currentUser.value.name}</span>}
        <button class="tb" onClick={() => void logout()}>
          Sign out
        </button>
        <button
          class="tb icon"
          title={theme.value === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
          aria-label="Toggle theme"
          onClick={toggleTheme}
        >
          <Icon name="theme" size={16} />
        </button>
      </header>
      <div class="body">
        <h1>Projects</h1>
        <p class="sub">Every project is a git repository on this server. Open one, or start a new one.</p>
        {error && <div class="err">{error}</div>}

        {!empty && (
          <div class="bar">
            {creating ? (
              <form
                style={{ display: 'flex', gap: 8 }}
                onSubmit={(e) => {
                  e.preventDefault();
                  void create();
                }}
              >
                <input
                  ref={input}
                  placeholder="Project name"
                  value={name}
                  maxLength={120}
                  onInput={(e) => setName((e.target as HTMLInputElement).value)}
                  onKeyDown={(e) => e.key === 'Escape' && setCreating(false)}
                  aria-label="Project name"
                />
                {templates.length > 1 && (
                  <select value={picked} aria-label="Template" onChange={(e) => setPicked((e.target as HTMLSelectElement).value)}>
                    {templates.map((t) => (
                      <option key={t.id} value={t.id}>
                        {t.name}
                      </option>
                    ))}
                  </select>
                )}
                <button class="tb primary" type="submit" disabled={!name.trim()}>
                  Create
                </button>
                <button class="tb" type="button" onClick={() => setCreating(false)}>
                  Cancel
                </button>
              </form>
            ) : (
              <button class="tb primary" onClick={() => start('blank-article')}>
                <Icon name="plus" size={14} /> New project
              </button>
            )}
            {(projects?.length ?? 0) > 6 && (
              <input placeholder="Search projects" value={query} onInput={(e) => setQuery((e.target as HTMLInputElement).value)} aria-label="Search projects" />
            )}
          </div>
        )}

        {projects === null && !error && <div class="empty">Loading…</div>}

        {empty && (
          <>
            <div class="empty" style={{ padding: '8px 0 0' }}>
              <b>Create a project</b>
              Start from a template. Everything in it is ordinary LaTeX you can rewrite.
            </div>
            <div class="choices">
              {templates.map((t) =>
                creating && picked === t.id ? (
                  <form
                    key={t.id}
                    class="choice"
                    onSubmit={(e) => {
                      e.preventDefault();
                      void create();
                    }}
                  >
                    <b>{t.name}</b>
                    <div class="inline-form" style={{ padding: '4px 0 0' }}>
                      {nameForm}
                    </div>
                  </form>
                ) : (
                  <button key={t.id} class="choice" onClick={() => start(t.id)}>
                    <b>{t.name}</b>
                    <span>{t.description}</span>
                  </button>
                ),
              )}
              <button class="choice" disabled title="Not available yet">
                <b>Import Overleaf zip</b>
                <span>Bring a project over with its history intact. Not available yet.</span>
              </button>
            </div>
          </>
        )}

        {shown.length > 0 && (
          <div class="grid">
            {shown.map((p) => (
              <button key={p.id} class="pcard" onClick={() => navigate(`/p/${p.id}`)}>
                <div class="t">{p.name}</div>
                <div class="d">
                  <span class="dot" /> edited {ago(p.updated_at)}
                </div>
              </button>
            ))}
          </div>
        )}
        {projects !== null && projects.length > 0 && shown.length === 0 && (
          <div class="empty">No project matches “{query}”.</div>
        )}
      </div>
    </div>
  );
}
