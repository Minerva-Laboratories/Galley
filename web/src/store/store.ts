// The only mutable state in the app. Components read signals. Actions live next to them.
import { computed, signal } from '@preact/signals';
import type { EditorView } from '@codemirror/view';
import type { AuthUser, BuildResult, Checkpoint, Comment, CommitInfo, FileEntry, PdfLocation, ProjectMeta, Role2, Suggestion } from '../api';
import type { DocSession } from '../sync/docs';

export type Theme = 'light' | 'dark';
export type Route = { kind: 'projects' } | { kind: 'editor'; id: string } | { kind: 'landing'; token: string };
export type DrawerName = 'files' | 'outline' | 'comments' | 'history' | 'tasks' | 'bib' | 'submit' | 'agents';
export type Connection = 'connecting' | 'online' | 'offline';

export interface Peer {
  clientId: number;
  name: string;
  color: string;
}

export interface ToastState {
  message: string;
  action?: { label: string; run: () => void };
}

function readPref(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writePref(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Preferences are a convenience. Blocked storage is not an error.
  }
}

function systemTheme(): Theme {
  return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches
    ? 'dark'
    : 'light';
}

// ---- preferences -------------------------------------------------------------------------

export const theme = signal<Theme>((readPref('galley.theme') as Theme | null) ?? systemTheme());
export function setTheme(next: Theme) {
  theme.value = next;
  writePref('galley.theme', next);
}
export function toggleTheme() {
  setTheme(theme.value === 'dark' ? 'light' : 'dark');
}

export const displayName = signal<string>(readPref('galley.name') ?? '');
export function setDisplayName(name: string) {
  const clean = name.trim().slice(0, 40);
  displayName.value = clean;
  writePref('galley.name', clean);
}

export const previewVisible = signal<boolean>(readPref('galley.preview') !== 'off');
export function togglePreview() {
  previewVisible.value = !previewVisible.value;
  writePref('galley.preview', previewVisible.value ? 'on' : 'off');
}

export const autoBuild = signal<boolean>(readPref('galley.autobuild') !== 'off');
export function setAutoBuild(on: boolean) {
  autoBuild.value = on;
  writePref('galley.autobuild', on ? 'on' : 'off');
}

export const draftMode = signal<boolean>(readPref('galley.draft') === 'on');
export function toggleDraft() {
  draftMode.value = !draftMode.value;
  writePref('galley.draft', draftMode.value ? 'on' : 'off');
}

export const invertPdf = signal<boolean>(readPref('galley.invert') === 'on');
export function toggleInvert() {
  invertPdf.value = !invertPdf.value;
  writePref('galley.invert', invertPdf.value ? 'on' : 'off');
}

// ---- routing -----------------------------------------------------------------------------

export function parseRoute(pathname: string): Route {
  const share = /^\/s\/([A-Za-z0-9_-]+)\/?$/.exec(pathname);
  if (share && share[1]) return { kind: 'landing', token: share[1] };
  const m = /^\/p\/([a-z0-9-]+)\/?$/.exec(pathname);
  return m && m[1] ? { kind: 'editor', id: m[1] } : { kind: 'projects' };
}

export const route = signal<Route>(parseRoute(location.pathname));

export function navigate(path: string) {
  history.pushState(null, '', path);
  route.value = parseRoute(path);
}

window.addEventListener('popstate', () => {
  route.value = parseRoute(location.pathname);
});

// ---- auth ---------------------------------------------------------------------------------

export const currentUser = signal<AuthUser | null>(null);
export const needsSetup = signal<boolean>(false);
export const publicSignup = signal<boolean>(false);
export const authReady = signal<boolean>(false);

export const suggestMode = signal<boolean>(readPref('galley.suggest') === 'on');
export function toggleSuggestMode() {
  suggestMode.value = !suggestMode.value;
  writePref('galley.suggest', suggestMode.value ? 'on' : 'off');
}

export const quietMode = signal<boolean>(false);
export function toggleQuietMode() {
  quietMode.value = !quietMode.value;
}

// ---- project / editor --------------------------------------------------------------------

export const project = signal<ProjectMeta | null>(null);
/** The current user's role on the open project. */
export const projectRole = signal<Role2 | null>(null);
export const comments = signal<Comment[]>([]);
export const suggestions = signal<Suggestion[]>([]);

const RANK: Record<Role2, number> = { viewer: 0, commenter: 1, editor: 2, admin: 3 };
export const canEdit = computed(() => (projectRole.value ? RANK[projectRole.value] >= RANK.editor : false));
export const canComment = computed(() => (projectRole.value ? RANK[projectRole.value] >= RANK.commenter : false));
export const canCompile = canComment;
export const isAdmin = computed(() => projectRole.value === 'admin');
/** Bumped when membership or governance requests change, so the Share modal re-fetches. */
export const sharingNonce = signal<number>(0);
export const files = signal<FileEntry[]>([]);
export const openTabs = signal<string[]>([]);
export const currentFile = signal<string | null>(null);
export const drawer = signal<DrawerName | null>('files');
export const connection = signal<Connection>('connecting');
export const peers = signal<Peer[]>([]);
export const commits = signal<CommitInfo[]>([]);
export const checkpoints = signal<Checkpoint[]>([]);
/** Whether the History timeline shows auto-commits. Off means checkpoints and edits only. */
export const showAllHistory = signal<boolean>(false);
/** When set, the full-screen History view is open, browsing this revision against current. */
export const historyView = signal<{ rev: string; label: string } | null>(null);
/** The full-screen citation graph. */
export const graphOpen = signal<boolean>(false);
/** Whether the server has latexdiff, enabling the PDF-comparison button. */
export const latexdiffAvailable = signal<boolean>(false);
export const wordCount = signal<number>(0);
export const paletteOpen = signal<boolean>(false);
export const shareOpen = signal<boolean>(false);
export const toast = signal<ToastState | null>(null);
/** The live document behind the active tab, and the CodeMirror view showing it. */
export const currentSession = signal<DocSession | null>(null);
export const editorView = signal<EditorView | null>(null);
/** A line to jump to once the editor for that file is mounted. */
export const pendingGoto = signal<{ file: string; line: number } | null>(null);

// ---- builds ------------------------------------------------------------------------------

export interface BuildState {
  /** idle means never built. running means a build is in progress, and last stays as it was. */
  phase: 'idle' | 'running' | 'done';
  progress: string | null;
  last: BuildResult | null;
}

export const build = signal<BuildState>({ phase: 'idle', progress: null, last: null });
/** Bumped whenever a fresh PDF should be loaded. 0 means nothing to show. */
export const pdfVersion = signal<number>(0);
export const problemsOpen = signal<boolean>(false);
export function toggleProblems() {
  problemsOpen.value = !problemsOpen.value;
}
/** Where the PDF should scroll to after a forward SyncTeX lookup. */
export const syncTarget = signal<(PdfLocation & { nonce: number }) | null>(null);

export function requestGoto(file: string, line: number) {
  openFile(file);
  const view = editorView.value;
  if (view && currentSession.value?.path === file) {
    pendingGoto.value = null;
    return { view, line };
  }
  pendingGoto.value = { file, line };
  return null;
}

export const textFiles = computed(() => files.value.filter((f) => f.kind === 'text'));

export function toggleDrawer(name: DrawerName) {
  drawer.value = drawer.value === name ? null : name;
}

export function openFile(path: string) {
  if (!openTabs.value.includes(path)) openTabs.value = [...openTabs.value, path];
  currentFile.value = path;
}

export function closeTab(path: string) {
  const rest = openTabs.value.filter((p) => p !== path);
  if (rest.length === 0) return;
  openTabs.value = rest;
  if (currentFile.value === path) currentFile.value = rest[rest.length - 1] ?? null;
}

/** A file went away: drop its tab, falling back to the main file when nothing else is open. */
export function forgetFile(path: string) {
  files.value = files.value.filter((f) => f.path !== path);
  const rest = openTabs.value.filter((p) => p !== path);
  const main = project.value?.main_file;
  if (rest.length === 0 && main && main !== path) rest.push(main);
  openTabs.value = rest;
  if (currentFile.value === path) currentFile.value = rest[rest.length - 1] ?? null;
}

/** A file moved: its tab follows it, and so does the main-file setting. */
export function movedFile(from: string, to: string) {
  const entry = files.value.find((f) => f.path === from);
  if (entry) {
    files.value = [...files.value.filter((f) => f.path !== from && f.path !== to), { ...entry, path: to }].sort((a, b) =>
      a.path.localeCompare(b.path),
    );
  }
  openTabs.value = openTabs.value.map((p) => (p === from ? to : p));
  if (currentFile.value === from) currentFile.value = to;
  if (project.value?.main_file === from) project.value = { ...project.value, main_file: to };
}

let toastTimer: ReturnType<typeof setTimeout> | undefined;
export function showToast(message: string, action?: ToastState['action']) {
  toast.value = { message, action };
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (toast.value = null), 4000);
}

export function resetProject() {
  project.value = null;
  projectRole.value = null;
  comments.value = [];
  suggestions.value = [];
  files.value = [];
  openTabs.value = [];
  currentFile.value = null;
  peers.value = [];
  commits.value = [];
  checkpoints.value = [];
  historyView.value = null;
  graphOpen.value = false;
  wordCount.value = 0;
  connection.value = 'connecting';
  currentSession.value = null;
  editorView.value = null;
  pendingGoto.value = null;
  build.value = { phase: 'idle', progress: null, last: null };
  pdfVersion.value = 0;
  problemsOpen.value = false;
  syncTarget.value = null;
}
