import { goToLine } from '../editor/Editor';
import type { Task } from '../editor/tasks';
import { canEdit, requestGoto } from '../store/store';
import { markDone, taskWho, tasks } from '../store/tasks';

export function TasksDrawer() {
  const all = tasks.value;
  const people = [...new Set(all.map((t) => t.who).filter(Boolean))];
  const who = taskWho.value;
  const list = all.filter((t) => !who || t.who === who);

  const goto = (t: Task) => {
    const hit = requestGoto(t.file, t.line);
    if (hit) goToLine(hit.view, hit.line);
  };

  return (
    <>
      <div class="dh">
        <span>Tasks</span>
        {people.length > 0 && (
          <select class="sel" value={who} onChange={(e) => (taskWho.value = (e.target as HTMLSelectElement).value)} aria-label="Assignee">
            <option value="">Everyone</option>
            {people.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        )}
      </div>
      <div class="db">
        {list.length === 0 && (
          <div class="empty">
            <b>No open tasks</b>
            Type todo and press Tab in the editor to add one.
          </div>
        )}
        {list.map((t) => (
          <div class="card" key={`${t.file}:${t.line}:${t.raw}`}>
            <div class="h">
              <span class={`lvl ${t.kind === 'FIXME' ? 'warning' : 'info'}`}>{t.kind}</span>
              {t.who && (
                <span class="av" style={{ width: 18, height: 18, fontSize: 9, margin: 0 }} title={t.who}>
                  {t.who.slice(0, 2).toUpperCase()}
                </span>
              )}
              <span class="loc" style={{ marginLeft: 'auto' }}>
                {t.file.split('/').pop()}:{t.line}
              </span>
            </div>
            <div class="d" style={{ color: 'var(--text)' }}>{t.text || <i>(no text)</i>}</div>
            <div class="acts">
              <button class="tb" onClick={() => goto(t)}>
                Go to
              </button>
              {canEdit.value && (
                <button class="tb" onClick={() => void markDone(t)}>
                  Done
                </button>
              )}
            </div>
          </div>
        ))}
        {list.length > 0 && <div class="hint">Add tasks with % TODO(@name): … or \todo{'{'}@name …{'}'}. Done removes the note from the source.</div>}
      </div>
    </>
  );
}
