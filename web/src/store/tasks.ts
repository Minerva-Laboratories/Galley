// Tasks (SPEC §13.6): live aggregation of TODO/FIXME notes across every .tex document, and Done,
// which removes the note through the CRDT and names the commit.
import { effect, signal } from '@preact/signals';
import { api } from '../api';
import { collectTasks, removalRange, type Task } from '../editor/tasks';
import { openDoc, type DocSession } from '../sync/docs';
import { messageOf } from './auth';
import { currentUser, displayName, files, project, showToast } from './store';

export const tasks = signal<Task[]>([]);
/** Assignee filter. Empty means everyone. */
export const taskWho = signal('');

/** Watch every .tex document of the open project. Returns a disposer. Sessions are shared with the
 *  editor, because openDoc caches them, so this adds no connections for files already open. */
export function watchTasks(): () => void {
  let observers: { s: DocSession; fn: () => void }[] = [];
  let timer: ReturnType<typeof setTimeout> | undefined;
  const stop = effect(() => {
    const id = project.value?.id;
    const list = files.value.filter((f) => f.kind === 'text' && f.path.endsWith('.tex'));
    for (const { s, fn } of observers) s.ytext.unobserve(fn);
    observers = [];
    if (!id) {
      tasks.value = [];
      return;
    }
    const name = displayName.peek() || currentUser.peek()?.name || 'Anonymous';
    const sessions = list.map((f) => openDoc(id, f.path, name));
    const refresh = () => {
      tasks.value = sessions.flatMap((s) => collectTasks(s.path, s.ytext.toString()));
    };
    const onChange = () => {
      clearTimeout(timer);
      timer = setTimeout(refresh, 250);
    };
    for (const s of sessions) {
      s.ytext.observe(onChange);
      observers.push({ s, fn: onChange });
      void s.ready.then(onChange);
    }
    refresh();
  });
  return () => {
    stop();
    clearTimeout(timer);
    for (const { s, fn } of observers) s.ytext.unobserve(fn);
  };
}

export async function markDone(task: Task): Promise<void> {
  const id = project.value?.id;
  if (!id) return;
  const name = displayName.value || currentUser.value?.name || 'Anonymous';
  const session = openDoc(id, task.file, name);
  await session.ready;
  const range = removalRange(session.ytext.toString(), task);
  if (!range) {
    showToast('That line changed; the note is no longer where it was.');
    return;
  }
  session.ytext.delete(range.from, range.to - range.from);
  showToast('Task removed from the source.');
  try {
    await api.flush(id, `done: ${task.text || task.kind.toLowerCase()}`.slice(0, 120));
  } catch (e) {
    showToast(messageOf(e, 'Removed; it will be committed with the next save.'));
  }
}
