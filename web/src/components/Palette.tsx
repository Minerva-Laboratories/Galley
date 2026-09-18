import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { commands } from '../editor/commands';
import { openFile, paletteOpen, textFiles } from '../store/store';
import { Icon } from './Icon';

interface Item {
  id: string;
  kind: 'file' | 'command';
  title: string;
  hint?: string;
  run: () => void;
}

/** Case-insensitive subsequence match. It is good enough for a few hundred items. */
export function matches(query: string, text: string): boolean {
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  let i = 0;
  for (const c of t) {
    if (c === q[i]) i++;
    if (i === q.length) return true;
  }
  return q.length === 0;
}

export function Palette() {
  const [query, setQuery] = useState('');
  const [index, setIndex] = useState(0);
  const input = useRef<HTMLInputElement>(null);

  const items = useMemo<Item[]>(() => {
    const fileItems: Item[] = textFiles.value.map((f) => ({
      id: `file:${f.path}`,
      kind: 'file',
      title: f.path,
      hint: 'Open',
      run: () => openFile(f.path),
    }));
    const commandItems: Item[] = commands
      .filter((c) => c.id !== 'palette')
      .map((c) => ({ id: `cmd:${c.id}`, kind: 'command', title: c.title, hint: c.keys, run: c.run }));
    const all = [...fileItems, ...commandItems];
    const q = query.trim();
    return q ? all.filter((i) => matches(q, i.title)) : all;
  }, [query, textFiles.value]);

  useEffect(() => {
    input.current?.focus();
  }, []);

  useEffect(() => {
    setIndex(0);
  }, [query]);

  const close = () => (paletteOpen.value = false);
  const pick = (item: Item | undefined) => {
    if (!item) return;
    close();
    item.run();
  };

  return (
    <div class="overlay" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div class="pal" role="dialog" aria-label="Command palette">
        <input
          ref={input}
          placeholder="Type a command, file, or section"
          autocomplete="off"
          value={query}
          onInput={(e) => setQuery((e.target as HTMLInputElement).value)}
          onKeyDown={(e) => {
            if (e.key === 'Escape') close();
            else if (e.key === 'ArrowDown') {
              e.preventDefault();
              setIndex((i) => Math.min(i + 1, items.length - 1));
            } else if (e.key === 'ArrowUp') {
              e.preventDefault();
              setIndex((i) => Math.max(i - 1, 0));
            } else if (e.key === 'Enter') pick(items[index]);
          }}
        />
        <div class="list" role="listbox">
          {items.length === 0 && <div class="empty">Nothing matches “{query}”.</div>}
          {items.map((item, i) => (
            <button
              key={item.id}
              class={`it ${i === index ? 'on' : ''}`}
              role="option"
              aria-selected={i === index}
              onMouseEnter={() => setIndex(i)}
              onClick={() => pick(item)}
            >
              <span class="k">{item.kind === 'file' ? 'File' : 'Command'}</span>
              {item.kind === 'file' && <Icon name="file" size={14} />}
              <span>{item.title}</span>
              {item.hint && <span class="g">{item.hint}</span>}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
