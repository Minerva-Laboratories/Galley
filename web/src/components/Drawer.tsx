import { useEffect, useRef, useState } from 'preact/hooks';
import type { Comment, Suggestion } from '../api';
import { goToLine } from '../editor/Editor';
import { sectionWords, type OutlineItem } from '../editor/latex';
import { setBudget } from '../store/settings';
import { acceptSuggestion, addComment, addSuggestion, rejectSuggestion, resolveComment, selection } from '../store/collab';
import { createCheckpoint, openHistory } from '../store/history';
import { renameLabel } from '../store/labels';
import { resolveAnchor } from '../sync/anchor';
import { openDoc } from '../sync/docs';
import { ago } from '../util/time';
import {
  canComment,
  canEdit,
  checkpoints,
  comments,
  commits,
  currentSession,
  currentUser,
  displayName,
  drawer,
  editorView,
  openFile,
  project,
  showToast,
  suggestions,
  suggestMode,
  toggleSuggestMode,
} from '../store/store';
import { AgentsDrawer } from './AgentsDrawer';
import { BibDrawer } from './BibDrawer';
import { FilesDrawer } from './FilesDrawer';
import { SubmitDrawer } from './SubmitDrawer';
import { TasksDrawer } from './TasksDrawer';

export function Drawer() {
  const which = drawer.value;
  if (!which) return null;
  return (
    <aside class="drawer" aria-label={TITLES[which]}>
      {which === 'files' && <FilesDrawer />}
      {which === 'outline' && <OutlineDrawer />}
      {which === 'comments' && <CommentsDrawer />}
      {which === 'history' && <HistoryDrawer />}
      {which === 'tasks' && <TasksDrawer />}
      {which === 'bib' && <BibDrawer />}
      {which === 'submit' && <SubmitDrawer />}
      {which === 'agents' && <AgentsDrawer />}
    </aside>
  );
}

const TITLES = { files: 'Files', outline: 'Outline', comments: 'Comments', history: 'History', tasks: 'Tasks', bib: 'Bibliography', submit: 'Submit', agents: 'Agents' } as const;

/** Jump to whatever text a comment or suggestion anchors to. */
function goToAnchor(file: string, anchor: string) {
  const id = project.value?.id;
  if (!id) return;
  openFile(file);
  const session = openDoc(id, file, displayName.value || currentUser.value?.name || 'Anonymous');
  const pos = resolveAnchor(session.ydoc, anchor);
  const view = editorView.value;
  if (view && currentSession.value?.path === file && pos !== null) {
    goToLine(view, view.state.doc.lineAt(Math.min(pos, view.state.doc.length)).number);
  }
}

function OutlineDrawer() {
  const session = currentSession.value;
  const [items, setItems] = useState<(OutlineItem & { words: number })[]>([]);

  useEffect(() => {
    if (!session) {
      setItems([]);
      return;
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    const refresh = () => setItems(sectionWords(session.ytext.toString()));
    const onChange = () => {
      clearTimeout(timer);
      timer = setTimeout(refresh, 200);
    };
    refresh();
    session.ytext.observe(onChange);
    return () => {
      clearTimeout(timer);
      session.ytext.unobserve(onChange);
    };
  }, [session]);

  return (
    <>
      <div class="dh">
        <span>Outline</span>
        {canEdit.value && (
          <button class="tb" onClick={() => void renameLabel()} title="Rename a label everywhere (Ctrl+Shift+R)">
            Rename label
          </button>
        )}
      </div>
      <div class="db">
        {!session && <div class="empty">Open a file to see its sections.</div>}
        {session && items.length === 0 && (
          <div class="empty">
            <b>No sections yet</b>
            Add a \section{'{'}…{'}'} and it appears here.
          </div>
        )}
        {items.map((it) => {
          const budget = project.value?.budgets?.[it.title];
          const pct = budget ? Math.min(100, Math.round((100 * it.words) / budget)) : 0;
          return (
            <div key={`${it.line}:${it.title}`}>
              <button class={`row lvl${it.level}`} onClick={() => editorView.value && goToLine(editorView.value, it.line)}>
                <span class="n">{it.title || '(untitled)'}</span>
                <span
                  class={`m ${canEdit.value ? 'b' : ''}`}
                  title={canEdit.value ? 'Click to set a word budget' : undefined}
                  onClick={(e) => {
                    if (!canEdit.value) return;
                    e.stopPropagation();
                    void setBudget(it.title);
                  }}
                >
                  {it.words}
                  {budget ? ` / ${budget}` : ''}
                </span>
              </button>
              {budget ? (
                <div class="budget">
                  <i class={it.words > budget ? 'over' : ''} style={{ width: `${pct}%` }} />
                </div>
              ) : null}
            </div>
          );
        })}
        {items.length > 0 && (
          <div class="hint">
            {items.reduce((a, b) => a + b.words, 0)} words in this file.{canEdit.value ? ' Click a count to set a budget.' : ''}
          </div>
        )}
      </div>
    </>
  );
}

function CommentsDrawer() {
  const [draft, setDraft] = useState('');
  const [suggesting, setSuggesting] = useState('');
  const [mode, setMode] = useState<'none' | 'comment' | 'suggest'>('none');
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (mode !== 'none') input.current?.focus();
  }, [mode]);

  const openSuggestions = suggestions.value.filter((s) => s.status === 'open');
  const openComments = comments.value.filter((c) => !c.resolved);
  const resolved = comments.value.filter((c) => c.resolved);
  const nothing = comments.value.length === 0 && suggestions.value.length === 0;

  const startComment = () => {
    const sel = selection();
    if (!sel) {
      showToast('Open a file first.');
      return;
    }
    setMode('comment');
  };
  const startSuggest = () => {
    const sel = selection();
    if (!sel || sel.from === sel.to) {
      showToast('Select the text you want to change first.');
      return;
    }
    setSuggesting(sel.text);
    setMode('suggest');
  };

  return (
    <>
      <div class="dh">
        <span>Comments</span>
        {canEdit.value && (
          <label class="chk" title="Track edits as suggestions instead of changing the text">
            <input type="checkbox" checked={suggestMode.value} onChange={toggleSuggestMode} /> Suggesting
          </label>
        )}
      </div>
      <div class="db">
        {canComment.value && (
          <div class="inline-form" style={{ flexWrap: 'wrap' }}>
            <button class="tb" onClick={startComment}>
              Comment on selection
            </button>
            <button class="tb" onClick={startSuggest}>
              Suggest edit
            </button>
          </div>
        )}
        {mode === 'comment' && (
          <form
            class="inline-form"
            onSubmit={(e) => {
              e.preventDefault();
              if (draft.trim()) void addComment(draft.trim());
              setDraft('');
              setMode('none');
            }}
          >
            <input ref={input} placeholder="Comment…" value={draft} style={{ fontFamily: 'var(--font)' }} onInput={(e) => setDraft((e.target as HTMLInputElement).value)} onKeyDown={(e) => e.key === 'Escape' && setMode('none')} />
            <button class="tb primary" type="submit" disabled={!draft.trim()}>
              Add
            </button>
          </form>
        )}
        {mode === 'suggest' && (
          <form
            class="inline-form"
            onSubmit={(e) => {
              e.preventDefault();
              void addSuggestion(suggesting);
              setMode('none');
            }}
          >
            <input ref={input} placeholder="Replacement text" value={suggesting} style={{ fontFamily: 'var(--mono)' }} onInput={(e) => setSuggesting((e.target as HTMLInputElement).value)} onKeyDown={(e) => e.key === 'Escape' && setMode('none')} />
            <button class="tb primary" type="submit">
              Suggest
            </button>
          </form>
        )}

        {nothing && (
          <div class="empty">
            <b>No comments yet</b>
            Select text and press “Comment on selection”. Comments stay anchored as the text moves.
          </div>
        )}

        {openSuggestions.length > 0 && <div class="hint">Suggestions</div>}
        {openSuggestions.map((s) => (
          <SuggestionCard key={s.id} s={s} />
        ))}

        {openComments.length > 0 && <div class="hint">Open</div>}
        {openComments.map((c) => (
          <CommentCard key={c.id} c={c} />
        ))}

        {resolved.length > 0 && <div class="hint">Resolved</div>}
        {resolved.map((c) => (
          <CommentCard key={c.id} c={c} />
        ))}
      </div>
    </>
  );
}

function CommentCard({ c }: { c: Comment }) {
  return (
    <div class={`card ${c.resolved ? 'resolved' : ''}`}>
      <div class="h">
        <span class="n" style={{ fontWeight: 500, color: 'var(--text)' }}>{c.author_name}</span>
        <span class="loc" style={{ marginLeft: 'auto' }}>{c.file.split('/').pop()}</span>
      </div>
      {c.quote && <div class="loc" style={{ marginTop: 4 }}>“{c.quote.slice(0, 60)}{c.quote.length > 60 ? '…' : ''}”</div>}
      <div class="t" style={{ fontWeight: 400, marginTop: 4 }}>{c.body}</div>
      <div class="acts">
        <button class="tb" onClick={() => goToAnchor(c.file, c.anchor)}>Go to</button>
        {canComment.value && (
          <button class="tb" onClick={() => void resolveComment(c.id, !c.resolved)}>{c.resolved ? 'Reopen' : 'Resolve'}</button>
        )}
      </div>
    </div>
  );
}

function SuggestionCard({ s }: { s: Suggestion }) {
  return (
    <div class="card">
      <div class="h">
        <span class="n" style={{ fontWeight: 500, color: 'var(--text)' }}>{s.author_name}</span>
        <span class="loc" style={{ marginLeft: 'auto' }}>{s.file.split('/').pop()}</span>
      </div>
      <div class="diff" style={{ marginTop: 6 }}>
        {s.quote && <div class="r">- {s.quote.slice(0, 80)}</div>}
        {s.replacement && <div class="a">+ {s.replacement.slice(0, 80)}</div>}
      </div>
      <div class="acts">
        <button class="tb" onClick={() => goToAnchor(s.file, s.anchor)}>Go to</button>
        {canEdit.value && <button class="tb" onClick={() => void acceptSuggestion(s)}>Accept</button>}
        <button class="tb" onClick={() => void rejectSuggestion(s.id)}>Reject</button>
      </div>
    </div>
  );
}

function HistoryDrawer() {
  const list = commits.value;
  const cpBySha = new Map(checkpoints.value.map((c) => [c.commit_sha, c]));
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);

  return (
    <>
      <div class="dh">
        <span>History</span>
        {canEdit.value && (
          <button class="tb" onClick={() => void createCheckpoint()} title="Create a checkpoint (Ctrl+Shift+C)">
            Checkpoint
          </button>
        )}
      </div>
      <div class="db">
        {list.length === 0 && (
          <div class="empty">
            <b>No commits yet</b>
            Edits are committed automatically a few seconds after you stop typing.
          </div>
        )}
        {list.length > 0 && (
          <div class="tl">
            {list.map((c) => {
              const cp = cpBySha.get(c.sha);
              const isAuto = !cp && c.message.startsWith('edit:');
              return (
                <div
                  key={c.sha}
                  class={`ti ${cp ? 'cp' : isAuto ? 'auto' : ''}`}
                  title="Click to compare with the current version"
                  onClick={() => openHistory(c.sha, cp ? cp.label : c.message)}
                >
                  <div class="l">{cp ? cp.label : c.message}</div>
                  <div class="s">
                    <span class="sha">{c.short_sha}</span>
                    <span>{c.author}</span>
                    <span>{ago(c.time, now)}</span>
                  </div>
                </div>
              );
            })}
          </div>
        )}
        {list.length > 0 && (
          <div class="hint">
            Click a version to compare it with the current text and restore it. Restoring creates a new commit;
            history is never rewritten.
          </div>
        )}
      </div>
    </>
  );
}
