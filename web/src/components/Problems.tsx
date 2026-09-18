import { useState } from 'preact/hooks';
import { api, ApiError, type BuildResult, type Diagnostic } from '../api';
import { goToLine } from '../editor/Editor';
import { build as runBuild } from '../editor/commands';
import { applyFix } from '../editor/fixes';
import { clearFigureCache, toggleFigureCache } from '../store/settings';
import { build, canCompile, canEdit, currentFile, problemsOpen, project, requestGoto, showToast } from '../store/store';

export function Problems() {
  const [grammar, setGrammar] = useState<Diagnostic[]>([]);
  const [checking, setChecking] = useState(false);
  const state = build.value;
  const last = state.last;
  const all = last?.errors ?? [];
  const shown = all.filter((d) => d.level === 'error' || d.level === 'warning');
  const lints = [...all.filter((d) => d.level === 'lint'), ...grammar];
  const notes = all.length - shown.length - lints.length;
  const title =
    shown.length > 0 || lints.length > 0
      ? `Problems${shown.length ? ` · ${shown.length}` : ''}${lints.length ? ` · lint ${lints.length}` : ''}`
      : 'Problems';

  const proofread = async () => {
    const id = project.value?.id;
    const path = currentFile.value;
    if (!id || !path) return;
    setChecking(true);
    try {
      const found = await api.checkGrammar(id, path);
      setGrammar(found);
      showToast(found.length === 0 ? `No grammar problems in ${path}.` : `${found.length} grammar ${found.length === 1 ? 'note' : 'notes'} in ${path}.`);
    } catch (e) {
      showToast(e instanceof ApiError ? e.message : 'Grammar checking is unavailable.');
    } finally {
      setChecking(false);
    }
  };

  const goto = (d: Diagnostic) => {
    if (!d.file || !d.line) return;
    const hit = requestGoto(d.file, d.line);
    if (hit) goToLine(hit.view, hit.line);
  };

  const fix = (d: Diagnostic) => {
    if (!d.fix) return;
    const outcome = applyFix(d.fix);
    if (outcome.ok) {
      showToast('Fix applied. Rebuilding.');
      void runBuild();
    } else {
      showToast(outcome.message);
    }
  };

  const card = (d: Diagnostic, i: number) => (
      <div class="card" key={`${d.code}-${d.file}-${d.line}-${i}`}>
        <div class="h">
          <span class={`lvl ${d.level}`}>{d.level}</span>
          {d.file && (
            <span class="loc">
              {d.file}
              {d.line ? `:${d.line}` : ''}
            </span>
          )}
          <span class="loc" style={{ marginLeft: 'auto' }}>
            {d.code}
          </span>
        </div>
        <div class="t">{d.message}</div>
        {d.hint && <div class="d">{d.hint}</div>}
        <div class="acts">
          {d.file && d.line && (
            <button class="tb" onClick={() => goto(d)}>
              Go to line
            </button>
          )}
          {d.fix && (
            <button class="tb" onClick={() => fix(d)}>
              {d.fix.label}
            </button>
          )}
          {d.explain && (
            <button class="tb" onClick={() => showToast(d.explain ?? '')}>
              Explain
            </button>
          )}
          {d.level === 'error' && !d.fix && (
            <button class="tb" title="Agents arrive in a later milestone" onClick={() => showToast('The fix-build agent arrives in a later milestone.')}>
              Ask Galley
            </button>
          )}
        </div>
      </div>
  );

  return (
    <div class="probs" role="region" aria-label="Problems">
      <div class="dh">
        <span>{title}</span>
        <span style={{ display: 'flex', gap: 6 }}>
          {project.value && currentFile.value?.endsWith('.tex') && (
            <button class="tb" disabled={checking} onClick={() => void proofread()}>
              {checking ? 'Checking…' : 'Check grammar'}
            </button>
          )}
          {project.value && last && (
            <a class="tb" href={api.logUrl(project.value.id)} target="_blank" rel="noreferrer">
              Show log
            </a>
          )}
          <button class="tb" onClick={() => (problemsOpen.value = false)}>
            Close
          </button>
        </span>
      </div>
      <div class="db">
        {last?.message && (
          <div class="card">
            <div class="h">
              <span class="lvl error">{last.status === 'timeout' ? 'timeout' : 'build'}</span>
            </div>
            <div class="t">{last.message}</div>
          </div>
        )}
        {shown.map(card)}
        {lints.length > 0 && (
          <div class="hint" style={{ gridColumn: '1 / -1', padding: '4px 0 0' }}>
            Lint · style, hygiene and grammar, never blocks a build
          </div>
        )}
        {lints.map(card)}
        {shown.length === 0 && !last?.message && (
          <div class="empty" style={{ gridColumn: '1 / -1' }}>
            <b>{state.phase === 'idle' && !last ? 'Nothing built yet' : 'No errors or warnings'}</b>
            {state.phase === 'idle' && !last ? 'Press Ctrl+Enter to build.' : 'The last build compiled cleanly.'}
          </div>
        )}
        {last?.figures && <FigureLine stats={last.figures} ms={last.profile.figure_ms ?? 0} />}
        {notes > 0 && (
          <div class="hint" style={{ gridColumn: '1 / -1' }}>
            {notes} informational {notes === 1 ? 'message' : 'messages'} in the log (fonts, reruns, empty bibliography).
          </div>
        )}
      </div>
    </div>
  );
}

/** The profiler's figure phase in one line, with the cache toggle (SPEC §13.1–13.2). */
function FigureLine({ stats, ms }: { stats: NonNullable<BuildResult['figures']>; ms: number }) {
  const on = project.value?.figure_cache !== false;
  let text: string;
  if (stats.mode === 'cached') {
    text = `Figures: all ${stats.total} from the cache${ms ? `, ${(ms / 1000).toFixed(1)} s spent refreshing changed ones` : ''}.`;
  } else if (stats.mode === 'plain') {
    text = `Figures: drawn in full this build, ${stats.cached} of ${stats.total} cached`;
    text += stats.pending ? `, ${stats.pending} caching in the background.` : '.';
    if (stats.reason) text += ` ${stats.reason[0]!.toUpperCase()}${stats.reason.slice(1)}.`;
  } else {
    text = `Figure cache off: ${stats.reason ?? 'not available'}.`;
  }
  return (
    <div class="hint figline" style={{ gridColumn: '1 / -1' }}>
      <span>{text} Cached figures are reused until their code, the preamble, or a data file changes.</span>
      {canEdit.value && (
        <label>
          <input type="checkbox" checked={on} onChange={() => void toggleFigureCache()} /> Cache figures
        </label>
      )}
      {canCompile.value && on && stats.mode !== 'off' && (
        <button class="tb" onClick={() => void clearFigureCache()}>
          Clear
        </button>
      )}
    </div>
  );
}
