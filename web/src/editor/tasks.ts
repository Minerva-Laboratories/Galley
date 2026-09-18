// Tasks (SPEC §13.6): the pure parser and remover for `% TODO(@name): …`, `% FIXME(…)`, and
// `\todo{@name …}` notes, shared by the drawer and its tests.
export interface Task {
  file: string;
  line: number;
  kind: 'TODO' | 'FIXME';
  who: string;
  text: string;
  src: 'comment' | 'macro';
  /** The note exactly as written, so Done can remove it verbatim. */
  raw: string;
}

const COMMENT = /%\s*(TODO|FIXME)(?:\((@?\w+)\))?:?\s*(.*)$/;
const MACRO = /\\todo\{(@\w+)?\s*([^}]*)\}/g;

export function collectTasks(file: string, text: string): Task[] {
  const out: Task[] = [];
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i]!;
    const m = COMMENT.exec(l);
    if (m) {
      out.push({ file, line: i + 1, kind: m[1] as Task['kind'], who: (m[2] ?? '').replace('@', ''), text: m[3]!.trim(), src: 'comment', raw: m[0] });
    }
    for (const mm of l.matchAll(MACRO)) {
      out.push({ file, line: i + 1, kind: 'TODO', who: (mm[1] ?? '').replace('@', ''), text: mm[2]!.trim(), src: 'macro', raw: mm[0] });
    }
  }
  return out;
}

/** The range to delete so the note disappears: the whole line when the note is the line,
 *  otherwise only the note and the whitespace before it. Null when the line no longer matches. */
export function removalRange(text: string, task: Task): { from: number; to: number } | null {
  const lines = text.split('\n');
  const line = lines[task.line - 1];
  if (line === undefined) return null;
  const at = line.indexOf(task.raw);
  if (at < 0) return null;
  let start = 0;
  for (let i = 0; i < task.line - 1; i++) start += lines[i]!.length + 1;
  if (line.trim() === task.raw.trim()) {
    const hasNewline = task.line < lines.length;
    return hasNewline ? { from: start, to: start + line.length + 1 } : { from: Math.max(0, start - 1), to: start + line.length };
  }
  let from = at;
  while (from > 0 && (line[from - 1] === ' ' || line[from - 1] === '\t')) from--;
  return { from: start + from, to: start + at + task.raw.length };
}
