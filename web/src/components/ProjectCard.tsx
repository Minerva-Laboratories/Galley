import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { api, ApiError, type ProjectMeta } from '../api';
import { navigate } from '../store/store';
import { ago } from '../util/time';
import { ProjectThumbnail } from './ProjectThumbnail';

export function ProjectCard({ project, previews, onRename, onDelete }: {
  project: ProjectMeta & { role?: string };
  previews: boolean;
  onRename: (meta: ProjectMeta) => void;
  onDelete: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(project.name);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [keepPreview, setKeepPreview] = useState(previews);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => { if (editing) { input.current?.focus(); input.current?.select(); } }, [editing]);
  useLayoutEffect(() => {
    if (previews) { setKeepPreview(true); return; }
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      setKeepPreview(false);
      return;
    }
    // Keep the rendered page through the exit transition; reversing the toggle cancels cleanup.
    const timer = window.setTimeout(() => setKeepPreview(false), 300);
    return () => window.clearTimeout(timer);
  }, [previews]);
  const save = async () => {
    if (!name.trim() || saving) return;
    setSaving(true);
    setError(null);
    try {
      onRename(await api.updateSettings(project.id, { name: name.trim() }));
      setEditing(false);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not rename the project.');
    } finally { setSaving(false); }
  };
  return (
    <article class="pcard" aria-label={project.name}>
      <a class="pcard-link" href={`/p/${encodeURIComponent(project.id)}`} aria-label={`Open ${project.name}`}
        onClick={(e) => {
          // Real links preserve Ctrl/Cmd-click, Shift-click, middle-click/trackpad taps, and menus.
          if (e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey || e.defaultPrevented) return;
          e.preventDefault();
          navigate(e.currentTarget.getAttribute('href')!);
        }}>
        <div class={`project-preview ${previews ? 'is-visible' : ''}`} aria-hidden={!previews}>
          {(previews || keepPreview) && <ProjectThumbnail id={project.id} version={project.updated_at} />}
        </div>
        <div class="pcard-summary"><div class="t">{project.name}</div><div class="d">Edited {ago(project.updated_at)}</div></div>
      </a>
      {editing ? <form class="pcard-edit" onSubmit={(e) => { e.preventDefault(); void save(); }}>
        <input ref={input} aria-label="Project name" value={name} maxLength={120} disabled={saving}
          onInput={(e) => setName(e.currentTarget.value)}
          onKeyDown={(e) => { if (e.key === 'Escape' && !saving) { setEditing(false); setError(null); } }} />
        <button class="tb" type="submit" disabled={saving || !name.trim()}>{saving ? 'Saving…' : 'Save'}</button>
        <button class="tb" type="button" disabled={saving} onClick={() => { setEditing(false); setError(null); }}>Cancel</button>
        {error && <div class="err" role="alert">{error}</div>}
      </form> : (project.role === 'admin' || project.role === 'editor') && <div class="pcard-actions">
        <button class="tb" onClick={() => { setName(project.name); setEditing(true); }}>Rename</button>
        {project.role === 'admin' && <button class="tb" onClick={onDelete}>Delete</button>}
      </div>}
    </article>
  );
}
