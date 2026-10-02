import { useEffect, useRef, useState } from 'preact/hooks';
import { api, type EngineKind } from '../api';
import { build as runBuild } from '../editor/commands';
import { wordCount as countWords } from '../editor/latex';
import { saveSettings } from '../store/settings';
import { autoBuild, build, canCompile, canEdit, currentSession, engineAvailability, engineAvailabilityError, narrowPane, project, setAutoBuild, toggleProblems, wordCount } from '../store/store';

export const ENGINE_LABELS: Record<EngineKind, string> = {
  tectonic: 'Tectonic',
  pdflatex: 'pdfLaTeX',
  xelatex: 'XeLaTeX',
  lualatex: 'LuaLaTeX',
  latex: 'LaTeX',
};
const ENGINE_ORDER = Object.keys(ENGINE_LABELS) as EngineKind[];
export function engineLabel(engine: string | null | undefined): string {
  return engine && engine in ENGINE_LABELS ? ENGINE_LABELS[engine as EngineKind] : engine ?? 'Unknown';
}

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? '' : 's'}`;
}

export function statusText(state: typeof build.value): string {
  if (state.phase === 'running') return state.progress ?? 'Building…';
  const last = state.last;
  if (!last) return 'Not built yet';
  const secs = last.profile.total_ms > 0 ? ` ${(last.profile.total_ms / 1000).toFixed(1)} s` : '';
  const lints = last.errors.filter((d) => d.level === 'lint').length;
  const lint = lints ? ` · ${lints} lint` : '';
  if (last.status === 'ok') {
    const w = last.warning_count ? plural(last.warning_count, 'warning') : 'no warnings';
    return `Compiled${secs} · ${w}${lint}${last.stale ? ' · out of date' : ''}`;
  }
  if (last.status === 'timeout') return 'Build timed out';
  if (last.status === 'error') return 'Build could not run';
  const parts = [`Build failed · ${plural(last.error_count, 'error')}`];
  if (last.warning_count) parts.push(plural(last.warning_count, 'warning'));
  return parts.join(' · ') + lint;
}

export function BuildBar() {
  const session = currentSession.value;
  const state = build.value;
  const selected = project.value?.engine ?? 'tectonic';
  const inventory = engineAvailability.value;
  const [pending, setPending] = useState(false);
  const pendingRef = useRef(false);

  useEffect(() => {
    if (!session) {
      wordCount.value = 0;
      return;
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    const refresh = () => (wordCount.value = countWords(session.ytext.toString()));
    const onChange = () => {
      clearTimeout(timer);
      timer = setTimeout(refresh, 300);
    };
    refresh();
    session.ytext.observe(onChange);
    return () => {
      clearTimeout(timer);
      session.ytext.unobserve(onChange);
    };
  }, [session]);

  const changeEngine = async (engine: EngineKind) => {
    if (pendingRef.current || !canEdit.value || engine === selected) return;
    const entry = engineAvailability.value?.engines.find((item) => item.engine === engine);
    if (!entry?.available) return;
    pendingRef.current = true;
    setPending(true);
    try {
      await saveSettings({ engine });
    } finally {
      pendingRef.current = false;
      setPending(false);
    }
  };
  const retryAvailability = async () => {
    const id = project.value?.id;
    if (!id) return;
    engineAvailabilityError.value = false;
    try {
      const response = await api.listEngines(id);
      if (project.value?.id !== id) return;
      // Project metadata and live events own the selection; a slow retry only refreshes capability.
      engineAvailability.value = { ...response, selected: project.value.engine ?? response.selected };
    } catch {
      if (project.value?.id === id) engineAvailabilityError.value = true;
    }
  };

  const dot = state.phase === 'running' ? 'run' : state.last?.status === 'ok' ? 'ok' : state.last ? 'fail' : '';
  const words = wordCount.value;
  const engineNotes = inventory?.engines.filter((item) => !item.available || Boolean(item.reason)) ?? [];
  const selectedAvailability = inventory?.engines.find((item) => item.engine === selected);
  return (
    <footer class={`build ${state.last && state.last.status !== 'ok' && state.phase !== 'running' ? 'failed' : ''}`}>
      <span class={`dot ${dot}`} />
      <button class="st" onClick={toggleProblems} title="Show problems (Ctrl+Shift+M)">
        {statusText(state)}
      </button>
      {session && <span class="wc">{words} {words === 1 ? 'word' : 'words'}</span>}
      <div class="r">
        <span class="pane-switch narrow-only" role="group" aria-label="Show source or PDF">
          <button class={`tb ${narrowPane.value === 'source' ? 'on' : ''}`} aria-pressed={narrowPane.value === 'source'} onClick={() => (narrowPane.value = 'source')}>Source</button>
          <button class={`tb ${narrowPane.value === 'pdf' ? 'on' : ''}`} aria-pressed={narrowPane.value === 'pdf'} onClick={() => (narrowPane.value = 'pdf')}>PDF</button>
        </span>
        {!canEdit.value && <span class="pill wait" title="Your role is read-only">Read-only</span>}
        <span class="eng" title="Engine selected for the next build">Next: {engineLabel(selected)} · {session?.path ?? project.value?.main_file ?? 'main.tex'}</span>
        {selected === 'latex' && <span class="engine-note" title="LaTeX/DVI accepts EPS and PS sources and produces a PDF preview">DVI: EPS/PS supported; TikZ cache and automatic SVG conversion unavailable</span>}
        {canEdit.value && (
          <span class="compiler-control">
            <select aria-label="Compiler" value={selected} disabled={pending || !inventory} onChange={(e) => void changeEngine((e.target as HTMLSelectElement).value as EngineKind)}>
              {ENGINE_ORDER.map((engine) => {
                const item = inventory?.engines.find((candidate) => candidate.engine === engine);
                return <option key={engine} value={engine} title={[item?.version, item?.reason].filter(Boolean).join(' · ') || undefined} disabled={item?.available === false || (Boolean(inventory) && !item)}>{ENGINE_LABELS[engine]}{item?.available === false ? ' (unavailable)' : ''}</option>;
              })}
            </select>
            {engineNotes.length > 0 && <details class="engine-reasons"><summary>Compiler notes</summary><div class="engine-reasons-list">{engineNotes.map((item) => <div key={item.engine}><b>{engineLabel(item.engine)}:</b> {item.reason ?? 'This compiler is unavailable on the server.'}</div>)}</div></details>}
            {!inventory && (engineAvailabilityError.value
              ? <button class="tb" onClick={() => void retryAvailability()}>Retry compilers</button>
              : <span class="engine-note">Checking compilers…</span>)}
            {selectedAvailability && (!selectedAvailability.available || selectedAvailability.reason) && <span class="engine-note">{selectedAvailability.reason ?? 'Selected compiler unavailable'}</span>}
          </span>
        )}
        {canEdit.value && (
          <select aria-label="Build mode" value={autoBuild.value ? 'auto' : 'manual'} onChange={(e) => setAutoBuild((e.target as HTMLSelectElement).value === 'auto')}>
            <option value="auto">Auto build</option>
            <option value="manual">Manual</option>
          </select>
        )}
        {canCompile.value && <button class="tb primary" data-tour="build" onClick={() => void runBuild()} disabled={state.phase === 'running'}>Build <kbd>Ctrl ↵</kbd></button>}
      </div>
    </footer>
  );
}
