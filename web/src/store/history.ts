// History actions: create a checkpoint, browse a past version, and restore one. Restore lands as
// a new commit and the server re-seeds the live document, so the editor updates in place. There
// is nothing to reload.
import { api, ApiError, WORKDIR } from '../api';
import { build } from '../editor/commands';
import { canEdit, historyView, project, showToast } from './store';

export function openHistory(rev: string, label: string) {
  historyView.value = { rev, label };
}

export function closeHistory() {
  historyView.value = null;
}

export async function createCheckpoint() {
  const id = project.value?.id;
  if (!id || !canEdit.value) return;
  const label = window.prompt('Checkpoint label', 'Before rewriting the method section');
  if (label === null || !label.trim()) return;
  try {
    const cp = await api.createCheckpoint(id, label.trim());
    showToast(`Checkpoint “${cp.label}” created`);
  } catch (e) {
    showToast(e instanceof ApiError ? e.message : 'Could not create the checkpoint.');
  }
}

/** Build a marked-up PDF of the changes from `rev` to the current text and open it in a new tab. */
export async function comparePdf(rev: string, file?: string) {
  const id = project.value?.id;
  if (!id) return;
  showToast('Building the comparison PDF…');
  try {
    await api.latexdiff(id, rev, WORKDIR, file);
    // Cache-bust so a fresh comparison always loads.
    window.open(`${api.latexdiffPdfUrl(id)}?t=${Date.now()}`, '_blank', 'noopener');
  } catch (e) {
    showToast(e instanceof ApiError ? e.message : 'Could not build the comparison.');
  }
}

export async function restoreVersion(rev: string, label: string) {
  const id = project.value?.id;
  if (!id || !canEdit.value) return;
  if (!window.confirm(`Restore “${label}”? This creates a new commit; nothing is rewritten.`)) return;
  try {
    await api.restore(id, rev, label);
    historyView.value = null;
    showToast('Restored as a new commit. Nothing was rewritten.');
    void build();
  } catch (e) {
    showToast(e instanceof ApiError ? e.message : 'Could not restore that version.');
  }
}
