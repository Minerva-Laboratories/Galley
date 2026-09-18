// Inline decorations for comments and suggestions, recomputed from the store. Comments underline
// their quoted range. Open suggestions strike the original. In suggestion mode they also show the
// replacement inline, as the prototype does.
import { EditorView, Decoration, type DecorationSet, WidgetType } from '@codemirror/view';
import { StateEffect, StateField } from '@codemirror/state';
import type { Comment, Suggestion } from '../api';
import { resolveAnchor } from '../sync/anchor';
import type { DocSession } from '../sync/docs';

export const setCollabDeco = StateEffect.define<DecorationSet>();

export const collabField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) if (e.is(setCollabDeco)) deco = e.value;
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

class InsertWidget extends WidgetType {
  constructor(readonly text: string) {
    super();
  }
  override eq(other: InsertWidget) {
    return other.text === this.text;
  }
  override toDOM() {
    const span = document.createElement('span');
    span.className = 'cm-suggest-add';
    span.textContent = this.text;
    return span;
  }
}

export function buildCollabDeco(
  view: EditorView,
  session: DocSession,
  comments: Comment[],
  suggestions: Suggestion[],
  suggestMode: boolean,
): DecorationSet {
  const len = view.state.doc.length;
  const ranges: { from: number; to: number; deco: Decoration }[] = [];

  for (const c of comments) {
    if (c.file !== session.path || c.resolved) continue;
    const at = resolveAnchor(session.ydoc, c.anchor);
    if (at === null) continue;
    const end = Math.min(len, at + Math.max(1, c.quote?.length ?? 1));
    if (end > at) ranges.push({ from: at, to: end, deco: Decoration.mark({ class: 'cm-comment' }) });
  }

  for (const s of suggestions) {
    if (s.file !== session.path || s.status !== 'open') continue;
    const from = resolveAnchor(session.ydoc, s.anchor);
    const to = resolveAnchor(session.ydoc, s.anchor_end);
    if (from === null || to === null || to < from) continue;
    if (to > from) {
      ranges.push({ from, to, deco: Decoration.mark({ class: suggestMode ? 'cm-suggest-del' : 'cm-suggest' }) });
    }
    if (suggestMode && s.replacement) {
      ranges.push({ from: to, to, deco: Decoration.widget({ widget: new InsertWidget(s.replacement), side: 1 }) });
    }
  }

  ranges.sort((a, b) => a.from - b.from || a.to - b.to);
  return Decoration.set(
    ranges.map((r) => r.deco.range(r.from, r.to)),
    true,
  );
}
