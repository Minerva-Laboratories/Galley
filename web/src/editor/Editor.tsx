import { useEffect, useRef } from 'preact/hooks';
import { effect } from '@preact/signals';
import { EditorState } from '@codemirror/state';
import {
  EditorView,
  drawSelection,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
} from '@codemirror/view';
import { defaultKeymap, indentWithTab } from '@codemirror/commands';
import { bracketMatching, indentOnInput, StreamLanguage } from '@codemirror/language';
import { closeBrackets, closeBracketsKeymap } from '@codemirror/autocomplete';
import { highlightSelectionMatches, searchKeymap } from '@codemirror/search';
import { stex } from '@codemirror/legacy-modes/mode/stex';
import { yCollab, yUndoManagerKeymap } from 'y-codemirror.next';
import * as Y from 'yjs';
import type { DocSession } from '../sync/docs';
import { build, canEdit, comments, editorView, pendingGoto, quietMode, suggestions, suggestMode } from '../store/store';
import { buildCollabDeco, collabField, setCollabDeco } from './collabExt';
import { galleyHighlighting, galleyTheme } from './theme';
import { globalKeymap, showInPdf } from './commands';
import { autoLabels, expandSnippet } from './snippets';
import { diagField, diagGutter, pushDiagnostics } from './diagGutter';

/** Ctrl/Cmd+click on a line shows it in the PDF (forward SyncTeX). */
const syncOnClick = EditorView.domEventHandlers({
  click(event, view) {
    if (!(event.ctrlKey || event.metaKey)) return false;
    const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
    if (pos === null) return false;
    void showInPdf(view.state.doc.lineAt(pos).number);
    return true;
  },
});

export function Editor({ session }: { session: DocSession }) {
  const host = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!host.current) return;
    let cancelled = false;
    let view: EditorView | null = null;
    let undoManager: Y.UndoManager | null = null;
    let disposeDeco: (() => void) | null = null;
    let disposeQuiet: (() => void) | null = null;
    let disposeDiag: (() => void) | null = null;
    let repaint: (() => void) | null = null;

    // Mount only once the document's content has loaded (see DocSession.ready), so the editor
    // starts with the full text rather than racing the async load.
    void session.ready.then(() => {
      if (cancelled || !host.current) return;
      undoManager = new Y.UndoManager(session.ytext);
      view = new EditorView({
      state: EditorState.create({
        doc: session.ytext.toString(),
        extensions: [
          lineNumbers(),
          diagField,
          diagGutter,
          highlightActiveLineGutter(),
          highlightSpecialChars(),
          drawSelection(),
          indentOnInput(),
          bracketMatching(),
          closeBrackets(),
          highlightSelectionMatches(),
          EditorView.lineWrapping,
          StreamLanguage.define(stex),
          galleyTheme,
          galleyHighlighting,
          collabField,
          syncOnClick,
          EditorState.readOnly.of(!canEdit.value),
          EditorView.editable.of(canEdit.value),
          yCollab(session.ytext, session.provider.awareness, { undoManager }),
          autoLabels,
          keymap.of([
            { key: 'Tab', run: expandSnippet },
            ...globalKeymap,
            ...yUndoManagerKeymap,
            ...closeBracketsKeymap,
            ...defaultKeymap,
            ...searchKeymap,
            indentWithTab,
          ]),
        ],
      }),
      parent: host.current,
      });
      editorView.value = view;
      const boundView = view;

      // Rebuild comment/suggestion decorations when they (or suggestion mode) change. Positions
      // track ordinary edits automatically through the StateField's change mapping, so this code
      // does not observe Y.Text here. A dispatch inside a Y.Text mutation reenters yCollab and stops
      // local edits from syncing. The dispatch is deferred so it never runs mid-transaction.
      repaint = () => {
        const deco = buildCollabDeco(boundView, session, comments.value, suggestions.value, suggestMode.value);
        queueMicrotask(() => {
          if (!cancelled) boundView.dispatch({ effects: setCollabDeco.of(deco) });
        });
      };
      disposeDeco = effect(() => {
        void comments.value;
        void suggestions.value;
        void suggestMode.value;
        repaint?.();
      });
      disposeQuiet = effect(() => {
        boundView.dom.classList.toggle('quiet', quietMode.value);
      });
      // Gutter dots follow the last build. Deferred like the decorations so it never runs mid-update.
      disposeDiag = effect(() => {
        const diags = (build.value.last?.errors ?? []).filter((d) => d.file === session.path);
        queueMicrotask(() => {
          if (!cancelled) pushDiagnostics(boundView, diags);
        });
      });

      const jump = pendingGoto.value;
      if (jump && jump.file === session.path) {
        pendingGoto.value = null;
        goToLine(boundView, jump.line);
      } else if (canEdit.value) {
        boundView.focus();
      }
    });

    return () => {
      cancelled = true;
      disposeDeco?.();
      disposeQuiet?.();
      disposeDiag?.();
      if (view && editorView.value === view) editorView.value = null;
      view?.destroy();
      undoManager?.destroy();
    };
  }, [session]);

  return <div class="cm-host" ref={host} />;
}

/** Scroll the editor to a 1-based line and put the cursor there. */
export function goToLine(view: EditorView, line: number) {
  const n = Math.max(1, Math.min(line, view.state.doc.lines));
  const pos = view.state.doc.line(n).from;
  view.dispatch({
    selection: { anchor: pos },
    effects: EditorView.scrollIntoView(pos, { y: 'center' }),
  });
  view.focus();
}
