// One-click fixes from error cards are applied through the CRDT as the user's own edit, so they
// sync, undo, and commit like typing (SPEC.md §6.3).
import type { Fix } from '../api';
import { openDoc } from '../sync/docs';
import { displayName, project } from '../store/store';
import { lineEndOffset, lineRange } from './lines';

export interface FixOutcome {
  ok: boolean;
  message: string;
}

export function applyFix(fix: Fix): FixOutcome {
  const id = project.value?.id;
  if (!id) return { ok: false, message: 'No project is open.' };
  const session = openDoc(id, fix.file, displayName.value || 'Anonymous');
  const text = session.ytext.toString();

  if (fix.kind === 'insert') {
    const at = lineEndOffset(text, fix.after_line);
    if (at === null) return { ok: false, message: `${fix.file} has no line ${fix.after_line} any more. Apply the change by hand.` };
    if (text.includes(fix.text)) return { ok: true, message: 'Already applied.' };
    session.ydoc.transact(() => session.ytext.insert(at, `\n${fix.text}`));
    return { ok: true, message: `Inserted ${fix.text} in ${fix.file}` };
  }

  const range = lineRange(text, fix.line);
  if (!range) return { ok: false, message: `${fix.file} has no line ${fix.line} any more. Apply the change by hand.` };
  let re: RegExp;
  try {
    re = new RegExp(fix.find);
  } catch {
    return { ok: false, message: 'The fix pattern is invalid. Apply the change by hand.' };
  }
  const m = re.exec(range.text);
  if (!m) return { ok: false, message: `Line ${fix.line} of ${fix.file} changed since the build. Build again and retry.` };
  const from = range.from + m.index;
  session.ydoc.transact(() => {
    session.ytext.delete(from, m[0].length);
    session.ytext.insert(from, fix.text);
  });
  return { ok: true, message: `Replaced ${m[0]} with ${fix.text}` };
}
