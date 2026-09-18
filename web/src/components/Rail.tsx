import { comments, drawer, suggestions, toggleDrawer, type DrawerName } from '../store/store';
import { tasks } from '../store/tasks';
import { Icon } from './Icon';

const ITEMS: { name: DrawerName; title: string }[] = [
  { name: 'files', title: 'Files' },
  { name: 'outline', title: 'Outline' },
  { name: 'comments', title: 'Comments' },
  { name: 'history', title: 'History' },
  { name: 'tasks', title: 'Tasks' },
  { name: 'bib', title: 'Bibliography' },
  { name: 'submit', title: 'Submit' },
  { name: 'agents', title: 'Agents' },
];

export function Rail() {
  const openThreads =
    comments.value.filter((c) => !c.resolved).length + suggestions.value.filter((s) => s.status === 'open').length;
  return (
    <nav class="rail" aria-label="Panels">
      {ITEMS.map((it) => (
        <button
          key={it.name}
          class={drawer.value === it.name ? 'on' : ''}
          title={it.title}
          aria-label={it.title}
          aria-pressed={drawer.value === it.name}
          onClick={() => toggleDrawer(it.name)}
        >
          <Icon name={it.name} />
          {it.name === 'comments' && openThreads > 0 && <span class="badge" aria-hidden="true" />}
          {it.name === 'tasks' && tasks.value.length > 0 && <span class="badge" aria-hidden="true" />}
        </button>
      ))}
      <span class="sp" />
    </nav>
  );
}
