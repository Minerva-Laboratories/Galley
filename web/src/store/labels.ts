// Rename label across the project (SPEC §13.4). Every .tex file is rewritten through its Y.Text so
// collaborators see the change as an ordinary edit. The flush names the commit.
import { api } from '../api';
import { labelAt, renameInText } from '../editor/labels';
import { openDoc, type DocSession } from '../sync/docs';
import { messageOf } from './auth';
import { currentUser, displayName, editorView, files, project, showToast } from './store';

/** A session opened only for the rename may not have received the server's copy yet. `ready`
 *  covers IndexedDB only. Wait for the provider's sync with a cap so an offline client still runs. */
function whenSynced(s: DocSession): Promise<void> {
  if (s.provider.synced) return Promise.resolve();
  return new Promise((resolve) => {
    const t = setTimeout(done, 4000);
    function done() {
      clearTimeout(t);
      s.provider.off('sync', done);
      resolve();
    }
    s.provider.on('sync', done);
  });
}

function labelAtCursor(): string {
  const view = editorView.value;
  if (!view) return '';
  const head = view.state.selection.main.head;
  const line = view.state.doc.lineAt(head);
  return labelAt(line.text, head - line.from) ?? '';
}

export async function renameLabel(): Promise<void> {
  const id = project.value?.id;
  if (!id) return;
  const from = window.prompt('Label to rename', labelAtCursor() || 'sec:method')?.trim();
  if (!from) return;
  const to = window.prompt(`Rename “${from}” to`, from)?.trim();
  if (!to || to === from) return;

  const name = displayName.value || currentUser.value?.name || 'Anonymous';
  let count = 0;
  let touched = 0;
  for (const f of files.value) {
    if (f.kind !== 'text' || !f.path.endsWith('.tex')) continue;
    const session = openDoc(id, f.path, name);
    await session.ready;
    await whenSynced(session);
    const edits = renameInText(session.ytext.toString(), from, to);
    if (edits.length === 0) continue;
    session.ydoc.transact(() => {
      for (const e of [...edits].sort((a, b) => b.from - a.from)) {
        session.ytext.delete(e.from, e.to - e.from);
        session.ytext.insert(e.from, e.insert);
      }
    });
    count += edits.length;
    touched += 1;
  }
  if (count === 0) {
    showToast(`No occurrences of “${from}” found.`);
    return;
  }
  showToast(`Renamed ${count} occurrence${count === 1 ? '' : 's'} in ${touched} file${touched === 1 ? '' : 's'}.`);
  try {
    await api.flush(id, `rename label ${from} → ${to}`);
  } catch (e) {
    showToast(messageOf(e, 'The rename was applied but could not be committed yet; it will be on the next save.'));
  }
}
