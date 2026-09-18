// Actions reachable from the keyboard, the palette, and buttons alike. The keymap follows
// SPEC.md §8.5. The same bindings are installed inside CodeMirror and on the window.
import type { KeyBinding } from '@codemirror/view';
import { api, ApiError } from '../api';
import { createCheckpoint } from '../store/history';
import { renameLabel } from '../store/labels';
import { clearFigureCache, setDeadline, toggleFigureCache } from '../store/settings';
import {
  build as buildState,
  currentSession,
  draftMode,
  drawer,
  editorView,
  graphOpen,
  paletteOpen,
  project,
  showToast,
  syncTarget,
  toggleDraft,
  toggleDrawer,
  togglePreview,
  toggleProblems,
  toggleTheme,
  type DrawerName,
} from '../store/store';

export interface Command {
  id: string;
  title: string;
  keys?: string;
  run: () => void;
}

export const commands: Command[] = [
  { id: 'build', title: 'Build', keys: 'Ctrl ↵', run: () => void build() },
  { id: 'palette', title: 'Search files and commands', keys: 'Ctrl K', run: () => (paletteOpen.value = true) },
  { id: 'problems', title: 'Toggle problems', keys: 'Ctrl ⇧ M', run: toggleProblems },
  { id: 'preview', title: 'Toggle preview', keys: 'Ctrl ⇧ P', run: togglePreview },
  { id: 'sync', title: 'Show cursor line in PDF', run: () => void showInPdf() },
  { id: 'draft', title: 'Toggle draft mode', run: () => (toggleDraft(), void build()) },
  { id: 'drawer', title: 'Toggle drawer', keys: 'Ctrl B', run: () => toggleDrawer(drawer.value ?? 'files') },
  { id: 'files', title: 'Show files', run: () => showDrawer('files') },
  { id: 'outline', title: 'Show outline', run: () => showDrawer('outline') },
  { id: 'history', title: 'Show history', run: () => showDrawer('history') },
  { id: 'tasks', title: 'Open tasks', run: () => showDrawer('tasks') },
  { id: 'bib', title: 'Show bibliography', run: () => showDrawer('bib') },
  { id: 'citation-graph', title: 'Citation graph', run: () => (graphOpen.value = true) },
  { id: 'deadline', title: 'Set deadline', run: () => void setDeadline() },
  { id: 'submit', title: 'Open submission checks', run: () => showDrawer('submit') },
  { id: 'figure-cache', title: 'Toggle persistent figure cache', run: () => void toggleFigureCache() },
  { id: 'clear-figures', title: 'Clear figure cache', run: () => void clearFigureCache() },
  { id: 'checkpoint', title: 'Create checkpoint', keys: 'Ctrl ⇧ C', run: () => void createCheckpoint() },
  { id: 'rename-label', title: 'Rename label…', keys: 'Ctrl ⇧ R', run: () => void renameLabel() },
  { id: 'theme', title: 'Toggle dark theme', run: toggleTheme },
  {
    id: 'snippet',
    title: 'Insert snippet: fig / tab / eq / sec / sub / cite / todo + Tab',
    keys: 'editor',
    run: () => showToast('Type fig, tab, eq, sec, sub, cite, or todo and press Tab.'),
  },
];

function showDrawer(name: DrawerName) {
  drawer.value = name;
}

/** Ask the server for a build of the file you're viewing. The result arrives over the event socket. */
export async function build() {
  const id = project.value?.id;
  if (!id) return;
  try {
    await api.build(id, draftMode.value, currentSession.value?.path);
    if (buildState.value.phase !== 'running') {
      buildState.value = { ...buildState.value, phase: 'running', progress: null };
    }
  } catch (e) {
    showToast(e instanceof ApiError ? e.message : 'Could not start the build.');
  }
}

/** Forward SyncTeX: scroll the PDF to the line under the cursor (or a given line). */
export async function showInPdf(line?: number) {
  const id = project.value?.id;
  const session = currentSession.value;
  const view = editorView.value;
  if (!id || !session) return;
  const target = line ?? (view ? view.state.doc.lineAt(view.state.selection.main.head).number : 1);
  try {
    const loc = await api.synctexForward(id, session.path, target);
    syncTarget.value = { ...loc, nonce: Date.now() };
  } catch (e) {
    showToast(e instanceof ApiError ? e.message : 'Nothing to show for that line.');
  }
}

export const globalKeymap: KeyBinding[] = [
  { key: 'Mod-Enter', run: () => (void build(), true) },
  { key: 'Mod-k', run: () => ((paletteOpen.value = true), true) },
  { key: 'Mod-b', run: () => (toggleDrawer(drawer.value ?? 'files'), true) },
  { key: 'Mod-Shift-p', run: () => (togglePreview(), true) },
  { key: 'Mod-Shift-m', run: () => (toggleProblems(), true) },
  { key: 'Mod-Shift-c', run: () => (void createCheckpoint(), true) },
  { key: 'Mod-Shift-r', run: () => (void renameLabel(), true) },
];

/** Window-level fallback so the shortcuts work when the editor is not focused. */
export function handleWindowKey(e: KeyboardEvent): boolean {
  const mod = e.ctrlKey || e.metaKey;
  if (!mod) return false;
  const key = e.key.toLowerCase();
  if (key === 'k') paletteOpen.value = true;
  else if (key === 'b' && !e.shiftKey) toggleDrawer(drawer.value ?? 'files');
  else if (key === 'p' && e.shiftKey) togglePreview();
  else if (key === 'm' && e.shiftKey) toggleProblems();
  else if (key === 'c' && e.shiftKey) void createCheckpoint();
  else if (key === 'r' && e.shiftKey) void renameLabel();
  else if (key === 'enter') void build();
  else return false;
  e.preventDefault();
  return true;
}
