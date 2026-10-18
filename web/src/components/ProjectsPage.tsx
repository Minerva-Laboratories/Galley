import { useEffect, useRef, useState } from 'preact/hooks';
import { api, ApiError, type ImportReport, type ProjectMeta, type Template } from '../api';
import { logout } from '../store/auth';
import { currentUser, navigate, showToast, theme, toggleTheme } from '../store/store';
import { ProjectCard } from './ProjectCard';
import { AppearanceButton } from './Appearance';
import { Icon } from './Icon';

export function ProjectsPage() {
  const [projects, setProjects] = useState<(ProjectMeta & { role?: string })[] | null>(null);
  const [templates, setTemplates] = useState<Template[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [picked, setPicked] = useState('blank-article');
  const [name, setName] = useState('');
  const [query, setQuery] = useState('');
  const input = useRef<HTMLInputElement>(null);
  const zipInput = useRef<HTMLInputElement>(null);
  const [importing, setImporting] = useState(false);
  const [imported, setImported] = useState<{ id: string; report: ImportReport } | null>(null);
  const [previews, setPreviews] = useState(() => {
    try { return localStorage.getItem('galley.project-previews') !== 'false'; } catch { return true; }
  });
  const [deleting, setDeleting] = useState<ProjectMeta | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const deleteDialog = useRef<HTMLDialogElement>(null);
  const refresh = useRef<() => void>(() => {});
  const revision = useRef(0);

  useEffect(() => {
    document.title = 'Projects — Galley';
    let stopped = false;
    let inFlight = false;
    const load = async () => {
      if (inFlight) return;
      inFlight = true;
      const ticket = ++revision.current;
      try {
        const list = await api.listProjects();
        if (!stopped && ticket === revision.current) { setProjects(list); setError(null); }
      } catch (e) {
        if (!stopped && ticket === revision.current) setError(e instanceof ApiError ? e.message : 'Could not load projects.');
      } finally { inFlight = false; }
    };
    refresh.current = () => void load();
    const onVisible = () => { if (document.visibilityState === 'visible') void load(); };
    void load();
    // Membership can change while the recipient is on the dashboard, before they have any project
    // socket. Refresh in the background and on return so invitations don't need a manual reload.
    const timer = window.setInterval(onVisible, 10_000);
    window.addEventListener('focus', onVisible);
    document.addEventListener('visibilitychange', onVisible);
    api.listTemplates().then(setTemplates).catch(() => setTemplates([]));
    return () => {
      stopped = true;
      window.clearInterval(timer);
      window.removeEventListener('focus', onVisible);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, []);

  useEffect(() => {
    if (deleting) deleteDialog.current?.showModal();
    else deleteDialog.current?.close();
  }, [deleting]);

  const deleteSelected = async () => {
    if (!deleting || deleteBusy) return;
    const selected = deleting;
    setDeleteBusy(true);
    setDeleteError(null);
    try {
      await api.deleteProject(selected.id);
      revision.current++;
      setProjects((prev) => (prev ?? []).filter((p) => p.id !== selected.id));
      setDeleting(null);
      showToast(`Deleted ${selected.name}.`);
    } catch (e) {
      setDeleteError(e instanceof ApiError ? e.message : 'Could not delete the project.');
    } finally { setDeleteBusy(false); }
  };

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

  // An import opens straight away when there is nothing to decide. When the zip had several
  // documents, or something that will not compile here, the summary stays up so the choice is seen.
  const importZip = async (file: File) => {
    setImporting(true);
    setError(null);
    try {
      const { project, report } = await api.importProject(file);
      if (report.warnings.length > 0 || report.other_candidates.length > 0) {
        setImported({ id: project.id, report });
      } else {
        showToast(`Imported ${report.file_count} files. Compiling ${report.main_file}.`);
        navigate(`/p/${project.id}`);
      }
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not import that zip.');
    } finally {
      setImporting(false);
      if (zipInput.current) zipInput.current.value = '';
    }
  };

  const zipPicker = (
    <input
      ref={zipInput}
      type="file"
      accept=".zip,application/zip"
      style={{ display: 'none' }}
      onChange={(e) => {
        const f = (e.target as HTMLInputElement).files?.[0];
        if (f) void importZip(f);
      }}
    />
  );

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
        <AppearanceButton />
        <button
          class="tb icon"
          title={theme.value === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
          aria-label="Toggle theme"
          onClick={toggleTheme}
        >
          <Icon name={theme.value === 'light' ? 'sun' : 'theme'} size={16} />
        </button>
      </header>
      <div class="body">
        <h1>Projects</h1>
        <p class="sub">Every project is a git repository on this server. Open one, or start a new one.</p>
        {error && <div class="err">{error}</div>}
        {zipPicker}
        {imported && (
          <div class="import-summary">
            <b>Imported {imported.report.file_count} files</b>
            <p>
              Galley will compile <code>{imported.report.main_file}</code>. {imported.report.main_reason}
            </p>
            {imported.report.other_candidates.length > 0 && (
              <p>
                Other files that start a document: {imported.report.other_candidates.join(', ')}. To compile another one,
                open its menu in the file list and choose Set as main file.
              </p>
            )}
            {imported.report.warnings.map((w) => (
              <p key={w} class="warn">
                {w}
              </p>
            ))}
            {imported.report.converted.length > 0 && (
              <p>Converted from Latin-1 to UTF-8: {imported.report.converted.join(', ')}. Check accented characters.</p>
            )}
            <div style={{ display: 'flex', gap: 8 }}>
              <button class="tb primary" onClick={() => navigate(`/p/${imported.id}`)}>
                Open project
              </button>
              <button class="tb" onClick={() => setImported(null)}>
                Close
              </button>
            </div>
          </div>
        )}

        {/* Not while the list loads: an empty account gets the template cards instead, and a bar
            that shows and then vanishes is a button pulled from under the pointer. */}
        {(projects !== null || !!error) && !empty && (
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
              <>
                <button class="tb primary" onClick={() => start('blank-article')}>
                  <Icon name="plus" size={14} /> New project
                </button>
                <button
                  class="tb"
                  onClick={() => zipInput.current?.click()}
                  disabled={importing}
                  title="Upload a zip, such as Overleaf's Menu, Download, Source. The main file is found for you."
                >
                  {importing ? 'Importing…' : 'Import zip'}
                </button>
              </>
            )}
            {(projects?.length ?? 0) > 6 && (
              <input placeholder="Search projects" value={query} onInput={(e) => setQuery((e.target as HTMLInputElement).value)} aria-label="Search projects" />
            )}
          </div>
        )}

        {(projects !== null || error) && <div class="project-view-controls">
          <label><input type="checkbox" checked={previews} onChange={(e) => {
            const next = e.currentTarget.checked;
            setPreviews(next);
            try { localStorage.setItem('galley.project-previews', String(next)); } catch { /* Storage is optional. */ }
          }} /> Show previews</label>
          <button class="tb" onClick={() => refresh.current()}>Refresh projects</button>
        </div>}
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
              <button class="choice" onClick={() => zipInput.current?.click()} disabled={importing}>
                <b>{importing ? 'Importing…' : 'Import Overleaf zip'}</b>
                <span>In Overleaf, use Menu, then Download, then Source. Galley finds the main file for you.</span>
              </button>
            </div>
          </>
        )}

        {shown.length > 0 && (
          <div class="grid">
            {shown.map((p) => (
              <ProjectCard key={p.id} project={p} previews={previews} onRename={(meta) => {
                revision.current++;
                setProjects((prev) => (prev ?? []).map((x) => x.id === p.id ? { ...x, ...meta } : x));
              }} onDelete={() => { setDeleteError(null); setDeleting(p); }} />
            ))}
          </div>
        )}
        {projects !== null && projects.length > 0 && shown.length === 0 && (
          <div class="empty">No project matches “{query}”.</div>
        )}
      </div>
      <dialog ref={deleteDialog} class="project-delete-dialog" aria-label="Delete project"
        onCancel={(e) => { if (deleteBusy) e.preventDefault(); else setDeleting(null); }}>
        <h2>Delete {deleting?.name}?</h2>
        <p>This permanently deletes the project, its files, and its history for everyone. Download the source first if you want to keep a copy.</p>
        {deleteError && <p class="err" role="alert">{deleteError}</p>}
        <div class="project-dialog-actions">
          <button class="tb" disabled={deleteBusy} autoFocus onClick={() => setDeleting(null)}>Cancel</button>
          <button class="tb danger" disabled={deleteBusy} onClick={() => void deleteSelected()}>{deleteBusy ? 'Deleting…' : 'Delete project'}</button>
        </div>
      </dialog>
    </div>
  );
}
