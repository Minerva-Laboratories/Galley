// Line/offset arithmetic on plain text. Kept free of DOM and store imports so it is unit-testable.

/** Offset of the end of 1-based `line` in `text` (null when the line does not exist). */
export function lineEndOffset(text: string, line: number): number | null {
  const lines = text.split('\n');
  if (line < 1 || line > lines.length) return null;
  let offset = 0;
  for (let i = 0; i < line; i++) offset += lines[i]!.length + (i < line - 1 ? 1 : 0);
  return offset;
}

export function lineRange(text: string, line: number): { from: number; to: number; text: string } | null {
  const lines = text.split('\n');
  if (line < 1 || line > lines.length) return null;
  let from = 0;
  for (let i = 0; i < line - 1; i++) from += lines[i]!.length + 1;
  const current = lines[line - 1]!;
  return { from, to: from + current.length, text: current };
}
