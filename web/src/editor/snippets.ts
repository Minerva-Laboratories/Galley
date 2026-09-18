// Snippets with auto-labels (SPEC §13.3): the editor bindings. A trigger word plus Tab expands. While
// a label's `% auto` comment is present, the slug follows the nearest caption or heading on every
// local edit. Deleting the comment pins the label. The rules live in autolabel.ts.
import type { ChangeSpec } from '@codemirror/state';
import { EditorView } from '@codemirror/view';
import { autoLabelChanges, SNIPPETS } from './autolabel';
import { showToast } from '../store/store';

const TRIGGER = /(^|\s)(fig|tab|eq|sec|sub|cite|todo)$/;

/** Tab: expand the trigger word before the cursor, or fall through to the next binding. */
export function expandSnippet(view: EditorView): boolean {
  const { state } = view;
  const sel = state.selection.main;
  if (!sel.empty) return false;
  const line = state.doc.lineAt(sel.head);
  const m = TRIGGER.exec(state.sliceDoc(line.from, sel.head));
  if (!m) return false;
  const word = m[2]!;
  const sn = SNIPPETS[word]!;
  const from = sel.head - word.length;
  const at = from + sn.text.indexOf(sn.cursor) + sn.cursor.length;
  view.dispatch({
    changes: { from, to: sel.head, insert: sn.text },
    selection: { anchor: at },
    userEvent: 'input.snippet',
    scrollIntoView: true,
  });
  showToast(
    sn.text.includes('% auto')
      ? `Inserted ${word} snippet. The label follows the caption while “% auto” is present.`
      : `Inserted ${word} snippet.`,
  );
  return true;
}

/** Rewrites run only after local user edits: remote changes are already labelled by their author,
 *  and reacting to them would have two clients insert the same text concurrently. */
export const autoLabels = EditorView.updateListener.of((update) => {
  if (!update.docChanged) return;
  if (!update.transactions.some((tr) => tr.isUserEvent('input') || tr.isUserEvent('delete'))) return;
  const changes: ChangeSpec = autoLabelChanges(update.state.doc.toString());
  if ((changes as unknown[]).length === 0) return;
  // Dispatching inside a listener is not allowed. The label sits after the caption, so the
  // mapped selection stays where the user left it.
  queueMicrotask(() => update.view.dispatch({ changes, userEvent: 'autolabel' }));
});
