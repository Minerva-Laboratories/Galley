// Small LaTeX text utilities shared by the outline, the build bar, and later the linter.

export interface OutlineItem {
  level: 1 | 2 | 3;
  title: string;
  line: number;
}

const HEADING = /^\s*\\(section|subsection|subsubsection)\*?\{([^}]*)\}/;

export function outline(text: string): OutlineItem[] {
  const items: OutlineItem[] = [];
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const m = HEADING.exec(lines[i]!);
    if (!m) continue;
    const level = m[1] === 'section' ? 1 : m[1] === 'subsection' ? 2 : 3;
    items.push({ level, title: m[2]!.trim(), line: i + 1 });
  }
  return items;
}

/** The outline with the prose word count of each section's body (up to the next heading). */
export function sectionWords(text: string): (OutlineItem & { words: number })[] {
  const items = outline(text);
  const lines = text.split('\n');
  return items.map((it, i) => {
    const end = i + 1 < items.length ? items[i + 1]!.line - 1 : lines.length;
    return { ...it, words: wordCount(lines.slice(it.line, end).join('\n')) };
  });
}

/** Words of prose, with commands, comments, and math stripped the way the prototype does. */
export function wordCount(text: string): number {
  const stripped = text
    .replace(/(^|[^\\])%.*$/gm, '$1')
    .replace(/\$[^$]*\$/g, ' ')
    .replace(/\\[a-zA-Z@]+\*?(\[[^\]]*\])?(\{[^}]*\})?/g, ' ')
    .replace(/[{}$%~]/g, ' ');
  return stripped.split(/\s+/).filter((w) => /[\p{L}\p{N}]/u.test(w)).length;
}
