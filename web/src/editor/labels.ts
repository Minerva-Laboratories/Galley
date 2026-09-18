// Rename label (SPEC §13.4): the pure rewrite rule, shared by the action and its tests. Every
// referencing command is covered, and comma lists such as \cref{a,b} rename only the matching key.
const LABEL_CMD = /\\(label|ref|eqref|autoref|cref|Cref|pageref|nameref)\*?\{([^}]*)\}/g;
const REF_AT_CURSOR = /\\(?:label|ref|eqref|autoref|cref|Cref|pageref|nameref)\*?\{([^}]+)\}/g;

export interface TextEdit {
  from: number;
  to: number;
  insert: string;
}

/** Edits that rename `from` to `to` inside label commands, positions in UTF-16 units. */
export function renameInText(text: string, from: string, to: string): TextEdit[] {
  const edits: TextEdit[] = [];
  if (!from || from === to) return edits;
  for (const m of text.matchAll(LABEL_CMD)) {
    const inner = m[2]!;
    const innerStart = m.index! + m[0].length - inner.length - 1;
    let offset = 0;
    for (const key of inner.split(',')) {
      const trimmed = key.trim();
      if (trimmed === from) {
        const at = innerStart + offset + key.indexOf(trimmed);
        edits.push({ from: at, to: at + trimmed.length, insert: to });
      }
      offset += key.length + 1;
    }
  }
  return edits;
}

/** The label the cursor is on, when the line has a label or reference command spanning it. */
export function labelAt(line: string, column: number): string | null {
  for (const m of line.matchAll(REF_AT_CURSOR)) {
    if (column >= m.index! && column <= m.index! + m[0].length) return m[1]!.split(',')[0]!.trim();
  }
  const first = REF_AT_CURSOR.exec(line);
  REF_AT_CURSOR.lastIndex = 0;
  return first ? first[1]!.split(',')[0]!.trim() : null;
}
