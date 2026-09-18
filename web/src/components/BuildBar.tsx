import { useEffect } from 'preact/hooks';
import { build as runBuild } from '../editor/commands';
import { wordCount as countWords } from '../editor/latex';
import { autoBuild, build, canCompile, canEdit, currentSession, project, setAutoBuild, toggleProblems, wordCount } from '../store/store';

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? '' : 's'}`;
}

export function statusText(state: typeof build.value): string {
  if (state.phase === 'running') return state.progress ?? 'Building…';
  const last = state.last;
  if (!last) return 'Not built yet';
  const secs = (last.profile.total_ms / 1000).toFixed(1);
  const lints = last.errors.filter((d) => d.level === 'lint').length;
  const lint = lints ? ` · ${lints} lint` : '';
  if (last.status === 'ok') {
    const w = last.warning_count ? plural(last.warning_count, 'warning') : 'no warnings';
    return `Compiled ${secs} s · ${w}${lint}${last.stale ? ' · out of date' : ''}`;
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

  const dot =
    state.phase === 'running' ? 'run' : state.last?.status === 'ok' ? 'ok' : state.last ? 'fail' : '';
  const words = wordCount.value;
  return (
    <footer class={`build ${state.last && state.last.status !== 'ok' && state.phase !== 'running' ? 'failed' : ''}`}>
      <span class={`dot ${dot}`} />
      <button class="st" onClick={toggleProblems} title="Show problems (Ctrl+Shift+M)">
        {statusText(state)}
      </button>
      {session && (
        <span class="wc">
          {words} {words === 1 ? 'word' : 'words'}
        </span>
      )}
      <div class="r">
        {!canEdit.value && <span class="pill wait" title="Your role is read-only">Read-only</span>}
        <span class="eng" title="Building the file you're viewing">Tectonic · {currentSession.value?.path ?? project.value?.main_file ?? 'main.tex'}</span>
        {canEdit.value && (
          <select aria-label="Build mode" value={autoBuild.value ? 'auto' : 'manual'} onChange={(e) => setAutoBuild((e.target as HTMLSelectElement).value === 'auto')}>
            <option value="auto">Auto build</option>
            <option value="manual">Manual</option>
          </select>
        )}
        {canCompile.value && (
          <button class="tb primary" onClick={() => void runBuild()} disabled={state.phase === 'running'}>
            Build <kbd>Ctrl ↵</kbd>
          </button>
        )}
      </div>
    </footer>
  );
}
