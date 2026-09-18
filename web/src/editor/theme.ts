// CodeMirror theme bound to the design tokens, so it follows the light/dark switch.
import { EditorView } from '@codemirror/view';
import { HighlightStyle, syntaxHighlighting } from '@codemirror/language';
import { tags } from '@lezer/highlight';

export const galleyTheme = EditorView.theme({
  '&': {
    height: '100%',
    fontSize: '12.5px',
    backgroundColor: 'var(--bg)',
    color: 'var(--text)',
  },
  '.cm-scroller': {
    fontFamily: 'var(--mono)',
    lineHeight: '1.7',
    overflow: 'auto',
  },
  '.cm-content': {
    padding: '12px 0',
    caretColor: 'var(--text)',
  },
  '.cm-line': { padding: '0 16px' },
  '&.cm-focused': { outline: 'none' },
  '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--text)' },
  '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection':
    { backgroundColor: 'var(--sel)' },
  '.cm-activeLine': { backgroundColor: 'transparent' },
  '.cm-activeLineGutter': { backgroundColor: 'var(--bg-sunk)', color: 'var(--text-2)' },
  '.cm-gutters': {
    backgroundColor: 'var(--bg)',
    color: 'var(--text-3)',
    borderRight: '1px solid var(--line)',
    fontFamily: 'var(--mono)',
    minWidth: '56px',
  },
  '.cm-lineNumbers .cm-gutterElement': { padding: '0 10px 0 6px' },
  '.cm-matchingBracket': { backgroundColor: 'var(--sel)', outline: '1px solid var(--line-strong)' },
  '.cm-searchMatch': { backgroundColor: 'var(--amber-soft)' },
  '.cm-searchMatch.cm-searchMatch-selected': { backgroundColor: 'var(--amber)' },
  '.cm-panels': { backgroundColor: 'var(--bg-panel)', color: 'var(--text)', borderColor: 'var(--line)' },
  '.cm-panels.cm-panels-bottom': { borderTop: '1px solid var(--line)' },
  '.cm-panel input, .cm-panel button': {
    fontFamily: 'var(--font)',
    fontSize: '12px',
    border: '1px solid var(--line)',
    borderRadius: '5px',
    background: 'var(--bg)',
    color: 'var(--text)',
  },
  '.cm-tooltip': {
    backgroundColor: 'var(--bg-panel)',
    border: '1px solid var(--line)',
    color: 'var(--text)',
  },
  // Collaborator carets and name flags (y-codemirror.next).
  '.cm-ySelectionInfo': {
    fontFamily: 'var(--font)',
    fontSize: '10px',
    fontWeight: '500',
    lineHeight: '12px',
    padding: '0 4px',
    borderRadius: '3px',
    color: '#fff',
    opacity: '1',
    top: '-13px',
  },
  '.cm-ySelectionCaret': { marginLeft: '-1px', marginRight: '-1px' },
});

const highlight = HighlightStyle.define([
  { tag: tags.tagName, color: 'var(--cmd)' },
  { tag: tags.keyword, color: 'var(--cmd)' },
  { tag: tags.atom, color: 'var(--text-2)' },
  { tag: tags.bracket, color: 'var(--text-2)' },
  { tag: tags.comment, color: 'var(--cmt)', fontStyle: 'italic' },
  { tag: tags.special(tags.variableName), color: 'var(--math)' },
  { tag: tags.string, color: 'var(--math)' },
  { tag: tags.invalid, color: 'var(--red)' },
]);

export const galleyHighlighting = syntaxHighlighting(highlight);
