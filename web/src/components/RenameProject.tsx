import { useEffect, useRef, useState } from 'preact/hooks';
import { api, ApiError, type ProjectMeta, type ProjectView } from '../api';
import { showToast } from '../store/store';

/** A native dialog supplies focus trapping, Escape handling and focus restoration. */
export function RenameProject({ project, onRenamed, onClose }: {
  project: Pick<ProjectMeta, 'id' | 'name'>;
  onRenamed: (updated: ProjectView) => void;
  onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const pending = useRef(false);
  const [name, setName] = useState(project.name);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const clean = name.trim();
  const tooLong = Array.from(clean).length > 120;

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    input.current?.focus();
    input.current?.select();
    return () => element?.close();
  }, []);

  const save = async () => {
    if (pending.current || !clean || tooLong) return;
    if (clean === project.name) {
      onClose();
      return;
    }
    pending.current = true;
    setSaving(true);
    setError(null);
    try {
      const updated = await api.renameProject(project.id, clean);
      onRenamed(updated);
      showToast('Project renamed.');
      onClose();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not rename the project. Try again.');
    } finally {
      pending.current = false;
      setSaving(false);
    }
  };

  return (
    <dialog
      ref={dialog}
      class="modal rename-project"
      aria-label="Rename project"
      onCancel={(e) => {
        e.preventDefault();
        if (!pending.current) onClose();
      }}
      onClick={(e) => {
        if (e.target !== e.currentTarget || pending.current) return;
        const rect = e.currentTarget.getBoundingClientRect();
        if (e.clientX < rect.left || e.clientX > rect.right || e.clientY < rect.top || e.clientY > rect.bottom) onClose();
      }}
    >
      <div class="mh">Rename project</div>
      <form class="mb" onSubmit={(e) => { e.preventDefault(); void save(); }} aria-busy={saving}>
        <label for="rename-project-name">Project name</label>
        <div class="field">
          <input
            id="rename-project-name"
            ref={input}
            value={name}
            disabled={saving}
            required
            aria-describedby="rename-project-hint"
            aria-invalid={tooLong || !!error}
            onInput={(e) => { setName((e.target as HTMLInputElement).value); setError(null); }}
          />
        </div>
        <p id="rename-project-hint" class={tooLong ? 'err' : 'hint'}>Up to 120 characters.</p>
        {error && <div class="err" role="alert">{error}</div>}
        <div class="rename-project-actions">
          <button class="tb" type="button" disabled={saving} onClick={onClose}>Cancel</button>
          <button class="tb primary" type="submit" disabled={saving || !clean || tooLong}>
            {saving ? 'Saving…' : 'Save'}
          </button>
        </div>
      </form>
    </dialog>
  );
}
