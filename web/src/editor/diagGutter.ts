// Gutter dots for the open file's diagnostics (SPEC §13.5): red for errors, amber for warnings,
// a neutral dot for lint. The editor pushes the current file's diagnostics in with `setDiag`.
import { StateEffect, StateField } from '@codemirror/state';
import { EditorView, gutter, GutterMarker } from '@codemirror/view';
import type { Diagnostic, Level } from '../api';

class Dot extends GutterMarker {
  constructor(readonly level: Level) {
    super();
  }
  override eq(other: Dot) {
    return other.level === this.level;
  }
  override toDOM() {
    const el = document.createElement('span');
    el.className = `dg dg-${this.level}`;
    return el;
  }
}

const RANK: Record<Level, number> = { error: 3, warning: 2, info: 1, lint: 0 };

export const setDiag = StateEffect.define<Diagnostic[]>();

/** Maps a line number to the strongest level on that line. */
export const diagField = StateField.define<Map<number, Level>>({
  create: () => new Map(),
  update(value, tr) {
    let next = value;
    for (const e of tr.effects) {
      if (!e.is(setDiag)) continue;
      next = new Map();
      for (const d of e.value) {
        if (!d.line || d.level === 'info') continue;
        const prev = next.get(d.line);
        if (!prev || RANK[d.level] > RANK[prev]) next.set(d.line, d.level);
      }
    }
    return next;
  },
});

export const diagGutter = gutter({
  class: 'cm-diag',
  lineMarker(view, line) {
    const level = view.state.field(diagField).get(view.state.doc.lineAt(line.from).number);
    return level ? new Dot(level) : null;
  },
  lineMarkerChange: (update) => update.transactions.some((tr) => tr.effects.some((e) => e.is(setDiag))),
});

export function pushDiagnostics(view: EditorView, diags: Diagnostic[]) {
  view.dispatch({ effects: setDiag.of(diags) });
}
