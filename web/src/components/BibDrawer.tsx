import { useEffect, useState } from 'preact/hooks';
import { api, ApiError, type BibEntry, type Library } from '../api';
import { goToLine } from '../editor/Editor';
import { saveSettings } from '../store/settings';
import { canEdit, graphOpen, project, requestGoto, showToast } from '../store/store';

/** What each audit code means to a reader, in a word. */
const ISSUE_LABEL: Record<string, string> = {
  'bib-duplicate-entry': 'duplicate',
  'bib-incomplete-entry': 'incomplete',
  'bib-preprint': 'preprint',
  'bib-uncited-entry': 'uncited',
};
/** The fields the editor offers by name. Anything else the file holds is kept untouched. */
const EDITABLE = ['author', 'title', 'year', 'journal', 'booktitle', 'publisher', 'doi', 'eprint', 'url'];

function message(e: unknown, fallback: string): string {
  return e instanceof ApiError ? e.message : fallback;
}

export function BibDrawer() {
  const id = project.value?.id;
  const [library, setLibrary] = useState<Library | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState('');
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState<string | null>(null);

  const reload = async (projectId: string) => {
    try {
      setLibrary(await api.library(projectId));
      setError(null);
    } catch (e) {
      setError(message(e, 'Could not read the bibliography.'));
    }
  };

  useEffect(() => {
    if (id) void reload(id);
  }, [id]);

  if (!id) return null;
  const entries = library?.entries ?? [];
  const withIssues = entries.filter((e) => e.issues.length > 0).length;

  const add = async () => {
    const text = adding.trim();
    if (!text) return;
    setBusy(true);
    try {
      const looksLikeBibtex = text.startsWith('@');
      const r = await api.addEntry(id, looksLikeBibtex ? { bibtex: text } : { identifier: text });
      setAdding('');
      await reload(id);
      setOpen(r.key);
      showToast(`Added ${r.key} to ${r.file}. Cite it with \\cite{${r.key}}.`);
    } catch (e) {
      showToast(message(e, 'Could not add that entry.'));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <div class="dh">
        <span>Bibliography</span>
        {entries.length > 0 && (
          <button class="tb" onClick={() => (graphOpen.value = true)} title="See how your citations cluster">
            Graph
          </button>
        )}
      </div>
      <div class="db">
        {error && <div class="err">{error}</div>}
        {canEdit.value && (
          <form
            class="bibadd"
            onSubmit={(e) => {
              e.preventDefault();
              void add();
            }}
          >
            <input
              placeholder="DOI, arXiv id, or pasted BibTeX"
              value={adding}
              onInput={(e) => setAdding((e.target as HTMLInputElement).value)}
              aria-label="Add a bibliography entry"
            />
            <button class="tb primary" type="submit" disabled={busy || !adding.trim()}>
              {busy ? 'Adding…' : 'Add'}
            </button>
          </form>
        )}
        {!library && !error && <div class="hint">Reading the bibliography…</div>}
        {library && entries.length === 0 && (
          <div class="empty">
            <b>No entries yet</b>
            Paste a DOI or an arXiv id above and Galley fetches the rest.
          </div>
        )}
        {entries.length > 0 && (
          <div class="hint" style={{ paddingTop: 0 }}>
            {entries.length} entries · {entries.filter((e) => e.cited > 0).length} cited
            {withIssues > 0 ? ` · ${withIssues} to look at` : ' · nothing to fix'}
          </div>
        )}
        {library?.missing.length ? (
          <div class="card">
            <div class="h">
              <span class="lvl error">missing</span>
            </div>
            <div class="t">
              {library.missing.length === 1 ? 'A key is cited' : `${library.missing.length} keys are cited`} with no
              entry: {library.missing.join(', ')}
            </div>
            <div class="d">LaTeX prints [?] for these. Add the entry above, or fix the key in the text.</div>
          </div>
        ) : null}
        {entries.map((e) => (
          <EntryRow
            key={e.key}
            entry={e}
            projectId={id}
            expanded={open === e.key}
            onToggle={() => setOpen(open === e.key ? null : e.key)}
            onChanged={() => void reload(id)}
          />
        ))}
        {library && !library.literature && canEdit.value && entries.length > 0 && (
          <div class="hint">
            Catalogue lookups are off, so Galley will not go looking for work you may be missing. Adding an entry by
            DOI still works — that is one lookup you asked for.{' '}
            <button
              class="linkish"
              onClick={() =>
                void saveSettings({ literature: true }).then((ok) => ok && showToast('Catalogue lookups are on.'))
              }
            >
              Turn them on
            </button>
          </div>
        )}
      </div>
    </>
  );
}

function EntryRow({
  entry,
  projectId,
  expanded,
  onToggle,
  onChanged,
}: {
  entry: BibEntry;
  projectId: string;
  expanded: boolean;
  onToggle: () => void;
  onChanged: () => void;
}) {
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [suggestions, setSuggestions] = useState<[string, string][] | null>(null);
  const fields = new Map(entry.fields);
  const editable = canEdit.value;

  const value = (name: string) => draft[name] ?? fields.get(name) ?? '';
  const dirty = Object.entries(draft).some(([k, v]) => v !== (fields.get(k) ?? ''));

  const run = async (what: () => Promise<string>) => {
    setBusy(true);
    try {
      showToast(await what());
      setDraft({});
      setSuggestions(null);
      onChanged();
    } catch (e) {
      showToast(message(e, 'That did not work.'));
    } finally {
      setBusy(false);
    }
  };

  const save = () =>
    run(async () => {
      await api.editEntry(projectId, entry.key, draft);
      return `Saved ${entry.key}.`;
    });

  const remove = () =>
    run(async () => {
      try {
        await api.deleteEntry(projectId, entry.key);
      } catch (e) {
        if (e instanceof ApiError && e.status === 400 && window.confirm(`${e.message}\n\nDelete it anyway?`)) {
          await api.deleteEntry(projectId, entry.key, true);
        } else {
          throw e;
        }
      }
      return `Deleted ${entry.key}.`;
    });

  const merge = () =>
    run(async () => {
      const into = entry.duplicate_of!;
      const r = await api.mergeEntry(projectId, entry.key, into);
      return `Merged into ${into}; ${r.moved} citation${r.moved === 1 ? '' : 's'} moved.`;
    });

  const lookup = async () => {
    setBusy(true);
    try {
      const r = await api.lookupEntry(projectId, entry.key);
      setSuggestions(r.suggestions);
      if (r.suggestions.length === 0) showToast('The catalogues have nothing to add to this entry.');
    } catch (e) {
      showToast(message(e, 'The lookup failed.'));
    } finally {
      setBusy(false);
    }
  };

  const applySuggestions = () =>
    run(async () => {
      await api.editEntry(projectId, entry.key, Object.fromEntries(suggestions ?? []));
      return `Updated ${entry.key} from the catalogues.`;
    });

  return (
    <div class={`bibentry ${expanded ? 'open' : ''}`}>
      <button class="row bibrow" onClick={onToggle} aria-expanded={expanded}>
        <span class="n">
          <span class="k">{entry.key}</span>
          {fields.get('title') && <span class="ti">{fields.get('title')}</span>}
          <span class="sub">
            {[fields.get('year'), fields.get('journal') ?? fields.get('booktitle')].filter(Boolean).join(' · ')}
            {entry.cited > 0 ? ` · §${entry.sections.join(', §')}` : ''}
          </span>
        </span>
        <span class="tags">
          {entry.issues.map((i) => (
            <span key={i} class={`tag ${i === 'bib-uncited-entry' ? 'warn' : ''}`}>
              {ISSUE_LABEL[i] ?? i}
            </span>
          ))}
          {entry.cited > 1 && <span class="m">{entry.cited}×</span>}
        </span>
      </button>
      {expanded && (
        <div class="bibedit">
          {EDITABLE.filter((f) => editable || fields.get(f)).map((f) => (
            <label key={f}>
              <span>{f}</span>
              <input
                value={value(f)}
                readOnly={!editable}
                onInput={(e) => setDraft({ ...draft, [f]: (e.target as HTMLInputElement).value })}
              />
            </label>
          ))}
          {suggestions && suggestions.length > 0 && (
            <div class="bibsug">
              <b>From the catalogues</b>
              {suggestions.map(([f, v]) => (
                <span key={f}>
                  {f} = {v.length > 70 ? `${v.slice(0, 69)}…` : v}
                </span>
              ))}
              <button class="tb" disabled={busy} onClick={() => void applySuggestions()}>
                Apply these
              </button>
            </div>
          )}
          <div class="acts">
            <button
              class="tb"
              onClick={() => {
                const hit = requestGoto(entry.file, entry.line);
                if (hit) goToLine(hit.view, hit.line);
              }}
            >
              Open in {entry.file}
            </button>
            {editable && dirty && (
              <button class="tb primary" disabled={busy} onClick={() => void save()}>
                Save
              </button>
            )}
            {editable && entry.duplicate_of && (
              <button class="tb" disabled={busy} onClick={() => void merge()}>
                Merge into {entry.duplicate_of}
              </button>
            )}
            {editable && (entry.issues.includes('bib-preprint') || entry.issues.includes('bib-incomplete-entry')) && (
              <button class="tb" disabled={busy} onClick={() => void lookup()}>
                {busy ? 'Looking…' : 'Check the catalogues'}
              </button>
            )}
            {editable && (
              <button class="tb danger" disabled={busy} onClick={() => void remove()}>
                Delete
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
