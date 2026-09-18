import { useEffect, useRef, useState } from 'preact/hooks';
import { api, ApiError, type BuildResult } from '../api';
import { Editor } from '../editor/Editor';
import { build as runBuild, handleWindowKey } from '../editor/commands';
import { loadCollab, upsertComment, upsertSuggestion, markSuggestion } from '../store/collab';
import { watchTasks } from '../store/tasks';
import { closeAll, closeDoc, openDoc } from '../sync/docs';
import { subscribeEvents } from '../sync/events';
import {
  autoBuild,
  build,
  checkpoints,
  comments,
  commits,
  connection,
  graphOpen,
  historyView,
  latexdiffAvailable,
  currentFile,
  currentSession,
  displayName,
  files,
  forgetFile,
  movedFile,
  navigate,
  openFile,
  openTabs,
  paletteOpen,
  pdfVersion,
  peers,
  previewVisible,
  problemsOpen,
  project,
  projectRole,
  resetProject,
  shareOpen,
  sharingNonce,
  showToast,
} from '../store/store';
import { BuildBar } from './BuildBar';
import { Drawer } from './Drawer';
import { CitationGraph } from './CitationGraph';
import { HistoryView } from './HistoryView';
import { Palette } from './Palette';
import { Preview } from './Preview';
import { Problems } from './Problems';
import { Rail } from './Rail';
import { ShareModal } from './Share';
import { Tabs } from './Tabs';
import { TopBar } from './TopBar';

const AUTO_BUILD_DELAY_MS = 1500;

export function EditorPage({ id }: { id: string }) {
  const [error, setError] = useState<string | null>(null);

  // Load the project, its files, history, and last build, then subscribe to events.
  // Tasks follow the project and its file list through signals. One watcher per mounted page.
  useEffect(() => watchTasks(), []);

  useEffect(() => {
    resetProject();
    setError(null);
    let cancelled = false;
    (async () => {
      try {
        const [meta, list, log, cps, status] = await Promise.all([
          api.getProject(id),
          api.listFiles(id),
          api.history(id),
          api.checkpoints(id).catch(() => []),
          api.buildStatus(id).catch(() => null),
        ]);
        if (cancelled) return;
        project.value = meta;
        projectRole.value = meta.role;
        files.value = list;
        commits.value = log;
        checkpoints.value = cps;
        void loadCollab(id);
        document.title = `${meta.name} — Galley`;
        if (status) {
          build.value = { phase: status.running ? 'running' : status.last ? 'done' : 'idle', progress: null, last: status.last };
          if (status.last?.pdf_available) pdfVersion.value = 1;
          latexdiffAvailable.value = status.latexdiff;
        }
        const first = list.find((f) => f.path === meta.main_file) ?? list.find((f) => f.kind === 'text');
        if (first) openFile(first.path);
      } catch (e) {
        if (!cancelled) setError(e instanceof ApiError ? e.message : 'Could not load the project.');
      }
    })();
    const unsubscribe = subscribeEvents(id, (ev) => {
      switch (ev.type) {
        case 'commit': {
          const commit = { sha: ev.sha, short_sha: ev.short_sha, message: ev.message, author: ev.author, time: ev.time };
          commits.value = [commit, ...commits.value.filter((c) => c.sha !== commit.sha)];
          break;
        }
        case 'file_created':
          if (!files.value.some((f) => f.path === ev.path)) {
            files.value = [...files.value, { path: ev.path, kind: 'text' as const, size: 0 }].sort((a, b) =>
              a.path.localeCompare(b.path),
            );
          }
          break;
        case 'file_deleted':
          forgetFile(ev.path);
          closeDoc(ev.path);
          break;
        case 'file_renamed':
          movedFile(ev.from, ev.to);
          closeDoc(ev.from);
          break;
        case 'files_changed':
          void api
            .listFiles(id)
            .then((list) => (files.value = list))
            .catch(() => undefined);
          break;
        case 'build_started':
          build.value = { ...build.value, phase: 'running', progress: null };
          break;
        case 'build_progress':
          build.value = { ...build.value, phase: 'running', progress: ev.message };
          break;
        case 'build_finished': {
          const result: BuildResult = { ...ev };
          build.value = { phase: 'done', progress: null, last: result };
          if (result.pdf_fresh) pdfVersion.value = pdfVersion.value + 1;
          else if (result.pdf_available && pdfVersion.value === 0) pdfVersion.value = 1;
          if (result.status !== 'ok') {
            if (!problemsOpen.value) problemsOpen.value = true;
            showToast(
              result.status === 'failed' && result.pdf_available
                ? 'Build failed. The previous PDF is still shown.'
                : result.message ?? 'Build failed.',
            );
          }
          break;
        }
        case 'comment_added':
          upsertComment(ev.comment);
          break;
        case 'comment_resolved':
          comments.value = comments.value.map((c) => (c.id === ev.id ? { ...c, resolved: ev.resolved } : c));
          break;
        case 'suggestion_added':
          upsertSuggestion(ev.suggestion);
          break;
        case 'suggestion_updated':
          markSuggestion(ev.id, ev.status);
          break;
        case 'members_changed':
        case 'requests_changed':
          sharingNonce.value = sharingNonce.value + 1;
          break;
        case 'checkpoints_changed':
          void api.checkpoints(id).then((cps) => (checkpoints.value = cps)).catch(() => {});
          break;
      }
    });
    return () => {
      cancelled = true;
      unsubscribe();
      closeAll();
      resetProject();
      document.title = 'Galley';
    };
  }, [id]);

  // Attach the live document for the active tab and mirror its presence/connection state.
  const path = currentFile.value;
  useEffect(() => {
    if (!path) {
      currentSession.value = null;
      return;
    }
    const session = openDoc(id, path, displayName.value || 'Anonymous');
    currentSession.value = session;
    const { provider } = session;
    const onStatus = ({ status }: { status: string }) => {
      connection.value = status === 'connected' ? 'online' : status === 'connecting' ? 'connecting' : 'offline';
    };
    const onAwareness = () => {
      const own = provider.awareness.clientID;
      const seen = new Map<string, { clientId: number; name: string; color: string }>();
      for (const [clientId, state] of provider.awareness.getStates()) {
        if (clientId === own) continue;
        const user = (state as { user?: { name?: string; color?: string } }).user;
        if (!user?.name) continue;
        // One person with several tabs still counts once.
        if (!seen.has(user.name)) seen.set(user.name, { clientId, name: user.name, color: user.color ?? '#5b6470' });
      }
      peers.value = [...seen.values()];
    };
    provider.on('status', onStatus);
    provider.awareness.on('change', onAwareness);
    onStatus({ status: provider.wsconnected ? 'connected' : 'connecting' });
    onAwareness();
    return () => {
      provider.off('status', onStatus);
      provider.awareness.off('change', onAwareness);
    };
  }, [id, path]);

  // Auto-build: 1.5 s after the last local edit, if nothing is running (SPEC.md §6.1).
  const session = currentSession.value;
  const autoTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => {
    if (!session) return;
    const onUpdate = (_update: Uint8Array, origin: unknown) => {
      if (!autoBuild.value) return;
      // Only edits made in this tab schedule a build. Remote peers schedule their own.
      if (origin === session.provider || origin === session.persistence) return;
      clearTimeout(autoTimer.current);
      autoTimer.current = setTimeout(() => {
        if (autoBuild.value && build.value.phase !== 'running') void runBuild();
      }, AUTO_BUILD_DELAY_MS);
    };
    session.ydoc.on('update', onUpdate);
    return () => {
      session.ydoc.off('update', onUpdate);
      clearTimeout(autoTimer.current);
    };
  }, [session]);

  // Window-level shortcuts (the editor installs the same ones internally).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (paletteOpen.value) return;
      const target = e.target as HTMLElement | null;
      if (target?.closest('.cm-editor')) return;
      handleWindowKey(e);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const mainClass = `main ${previewVisible.value ? '' : 'no-preview'}`;

  if (error) {
    return (
      <div class="center">
        <div class="empty" style={{ textAlign: 'center' }}>
          <b>{error}</b>
          <button class="tb" style={{ margin: '10px auto 0' }} onClick={() => navigate('/')}>
            Back to projects
          </button>
        </div>
      </div>
    );
  }

  return (
    <div class={`app ${problemsOpen.value ? 'probs-open' : ''}`}>
      <TopBar />
      <div class={mainClass}>
        <Rail />
        <Drawer />
        <section class="editor">
          <Tabs />
          <div class="ed">
            {session ? (
              <Editor key={session.path} session={session} />
            ) : (
              <div class="ed-empty empty">{openTabs.value.length === 0 ? 'Open a file from the Files drawer.' : 'Loading…'}</div>
            )}
          </div>
        </section>
        <Splitter />
        <Preview />
      </div>
      {problemsOpen.value && <Problems />}
      <BuildBar />
      {historyView.value && <HistoryView id={id} />}
      {graphOpen.value && <CitationGraph id={id} />}
      {paletteOpen.value && <Palette />}
      {shareOpen.value && <ShareModal />}
    </div>
  );
}

/** Drag handle between editor and preview. It stores the ratio as CSS variables on the grid. */
function Splitter() {
  const dragging = useRef(false);
  const [active, setActive] = useState(false);
  useEffect(() => {
    const move = (e: MouseEvent) => {
      if (!dragging.current) return;
      const main = document.querySelector<HTMLElement>('.main');
      if (!main) return;
      const rect = main.getBoundingClientRect();
      const left = 48 + (document.querySelector<HTMLElement>('.drawer')?.offsetWidth ?? 0);
      const usable = rect.width - left - 6;
      const ratio = Math.min(0.8, Math.max(0.2, (e.clientX - rect.left - left) / usable));
      main.style.setProperty('--ed-fr', `${ratio}fr`);
      main.style.setProperty('--pv-fr', `${1 - ratio}fr`);
    };
    const up = () => {
      if (!dragging.current) return;
      dragging.current = false;
      setActive(false);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
    return () => {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
    };
  }, []);
  return (
    <div
      class={`split ${active ? 'dragging' : ''}`}
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize editor and preview"
      title="Drag to resize"
      onMouseDown={(e) => {
        e.preventDefault();
        dragging.current = true;
        setActive(true);
        document.body.style.cursor = 'col-resize';
        document.body.style.userSelect = 'none';
      }}
      onDblClick={() => {
        const main = document.querySelector<HTMLElement>('.main');
        main?.style.removeProperty('--ed-fr');
        main?.style.removeProperty('--pv-fr');
        showToast('Split reset');
      }}
    />
  );
}
