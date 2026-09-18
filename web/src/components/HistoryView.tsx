import { useEffect, useState } from 'preact/hooks';
import { api, WORKDIR, type Checkpoint, type CommitInfo, type FileDiff } from '../api';
import {
  canEdit,
  checkpoints,
  commits,
  currentFile,
  historyView,
  latexdiffAvailable,
  showAllHistory,
  textFiles,
} from '../store/store';
import { closeHistory, comparePdf, createCheckpoint, openHistory, restoreVersion } from '../store/history';
import { ago } from '../util/time';

interface Entry {
  sha: string;
  short_sha: string;
  label: string;
  author: string;
  time: string;
  kind: 'checkpoint' | 'edit' | 'restore';
}

/** Merge the commit log with checkpoint tags into one timeline. Auto-commits (`edit:`) are hidden
 *  unless "all" is on. Checkpoints and restores always show. */
function timeline(all: boolean): Entry[] {
  const byCommit = new Map<string, Checkpoint>();
  for (const cp of checkpoints.value) byCommit.set(cp.commit_sha, cp);
  const out: Entry[] = [];
  for (const c of commits.value as CommitInfo[]) {
    const cp = byCommit.get(c.sha);
    if (cp) {
      out.push({ sha: c.sha, short_sha: c.short_sha, label: cp.label, author: c.author, time: c.time, kind: 'checkpoint' });
    } else if (c.message.startsWith('restore:')) {
      out.push({ sha: c.sha, short_sha: c.short_sha, label: c.message, author: c.author, time: c.time, kind: 'restore' });
    } else if (all || !c.message.startsWith('edit:')) {
      out.push({ sha: c.sha, short_sha: c.short_sha, label: c.message, author: c.author, time: c.time, kind: 'edit' });
    }
  }
  return out;
}

export function HistoryView({ id }: { id: string }) {
  const view = historyView.value;
  const [file, setFile] = useState<string>(currentFile.value ?? '');
  const [diff, setDiff] = useState<FileDiff[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [now] = useState(Date.now());

  const entries = timeline(showAllHistory.value);

  // Fetch the diff of the selected revision against the live working tree for the chosen file.
  useEffect(() => {
    if (!view) return;
    let cancelled = false;
    setLoading(true);
    api
      .diff(id, view.rev, WORKDIR, file || undefined)
      .then((d) => {
        if (!cancelled) setDiff(d);
      })
      .catch(() => {
        if (!cancelled) setDiff([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [id, view?.rev, file]);

  if (!view) return null;
  const shown = file ? diff?.filter((d) => d.path === file) ?? [] : diff ?? [];

  return (
    <div class="hv-overlay" onClick={(e) => e.target === e.currentTarget && closeHistory()}>
      <div class="hv" role="dialog" aria-label="History">
        <div class="hv-head">
          <b>History</b>
          <span class="hv-note">
            Comparing <strong>{view.label}</strong> ({view.rev.slice(0, 7)}) with the current version.
            Editing stays live; restore creates a new commit.
          </span>
          <button class="tb" onClick={closeHistory}>
            Close
          </button>
        </div>
        <div class="hv-body">
          <aside class="hv-tl">
            <div class="hv-tools">
              {canEdit.value && (
                <button class="tb" onClick={() => void createCheckpoint()}>
                  Checkpoint
                </button>
              )}
              <label class="hv-all">
                <input
                  type="checkbox"
                  checked={showAllHistory.value}
                  onChange={(e) => (showAllHistory.value = (e.target as HTMLInputElement).checked)}
                />
                all
              </label>
            </div>
            {entries.length === 0 ? (
              <div class="empty">
                <b>No versions yet</b>
                Edits are committed automatically a few seconds after you stop typing.
              </div>
            ) : (
              <div class="tl">
                {entries.map((e) => (
                  <div
                    key={e.sha}
                    class={`ti ${e.kind === 'checkpoint' ? 'cp' : e.kind === 'edit' ? 'auto' : ''} ${
                      view.rev === e.sha ? 'on' : ''
                    }`}
                    title={e.sha}
                    onClick={() => openHistory(e.sha, e.label)}
                  >
                    <div class="l">{e.label}</div>
                    <div class="s">
                      <span class="sha">{e.short_sha}</span>
                      <span>{e.author}</span>
                      <span>{ago(e.time, now)}</span>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </aside>
          <main class="hv-diff">
            <div class="hv-diffhead">
              <select value={file} onChange={(e) => setFile((e.target as HTMLSelectElement).value)}>
                <option value="">All files</option>
                {textFiles.value.map((f) => (
                  <option key={f.path} value={f.path}>
                    {f.path}
                  </option>
                ))}
              </select>
              <span class="hv-spacer" />
              <button
                class="tb"
                disabled={!latexdiffAvailable.value}
                title={
                  latexdiffAvailable.value
                    ? 'Compile a marked-up PDF of the changes vs. the current version'
                    : 'This server has no latexdiff installed'
                }
                onClick={() => void comparePdf(view.rev, file || undefined)}
              >
                Compare PDFs
              </button>
              {canEdit.value && (
                <button class="tb primary" onClick={() => void restoreVersion(view.rev, view.label)}>
                  Restore this version
                </button>
              )}
            </div>
            <div class="hv-diffbody">
              {loading ? (
                <div class="empty">Loading diff…</div>
              ) : shown.length === 0 ? (
                <div class="empty">
                  <b>No differences</b>
                  This version matches the current text{file ? ` for ${file}` : ''}.
                </div>
              ) : (
                shown.map((fd) => <DiffFile key={fd.path} fd={fd} />)
              )}
            </div>
          </main>
        </div>
      </div>
    </div>
  );
}

function DiffFile({ fd }: { fd: FileDiff }) {
  return (
    <div class="diff">
      <div class="hd fname">
        {fd.status !== 'modified' ? `${fd.status}: ` : ''}
        {fd.old_path ? `${fd.old_path} → ${fd.path}` : fd.path}
      </div>
      {fd.binary ? (
        <div class="hd">Binary file — not shown.</div>
      ) : (
        fd.hunks.map((h, i) => (
          <div key={i}>
            {h.header && <div class="hd">{h.header}</div>}
            {h.lines.map((l, j) => (
              <div key={j} class={l.origin === '+' ? 'a' : l.origin === '-' ? 'r' : ''}>
                {l.origin === ' ' ? ' ' : l.origin}
                {l.content}
              </div>
            ))}
          </div>
        ))
      )}
    </div>
  );
}
