import { useEffect, useState } from 'preact/hooks';
import { api, type PackReport, type Venue } from '../api';
import { complianceChecks } from '../editor/compliance';
import { build as runBuild } from '../editor/commands';
import { applyFix } from '../editor/fixes';
import { messageOf } from '../store/auth';
import { comparePdf } from '../store/history';
import { saveSettings } from '../store/settings';
import {
  build,
  canCompile,
  canEdit,
  currentUser,
  displayName,
  files,
  latexdiffAvailable,
  project,
  showToast,
} from '../store/store';
import { tasks } from '../store/tasks';
import { openDoc } from '../sync/docs';

export function SubmitDrawer() {
  const meta = project.value;
  const [venues, setVenues] = useState<Venue[]>([]);
  const [texts, setTexts] = useState<{ main: string; others: string[] }>({
    main: '',
    others: [],
  });
  const [packing, setPacking] = useState(false);
  const [report, setReport] = useState<PackReport | null>(null);
  const last = build.value.last;

  useEffect(() => {
    api
      .venues()
      .then(setVenues)
      .catch(() => setVenues([]));
  }, []);

  // Read the live documents. Tasks already keep every .tex session open, so these are cached.
  useEffect(() => {
    if (!meta) return;
    const name = displayName.value || currentUser.value?.name || 'Anonymous';
    const tex = files.value.filter((f) => f.kind === 'text' && f.path.endsWith('.tex'));
    const read = () => {
      const main = openDoc(meta.id, meta.main_file, name).ytext.toString();
      const others = tex.filter((f) => f.path !== meta.main_file).map((f) => openDoc(meta.id, f.path, name).ytext.toString());
      setTexts({ main, others });
    };
    read();
    const t = setInterval(read, 1500);
    return () => clearInterval(t);
  }, [meta?.id, meta?.main_file, files.value]);

  if (!meta) return null;
  const venue = venues.find((v) => v.id === meta.venue) ?? venues[0];
  const checks = venue
    ? complianceChecks({
        mainFile: meta.main_file,
        mainText: texts.main,
        otherTex: texts.others,
        venue,
        pages: last?.pages ?? null,
        openTasks: tasks.value.length,
        buildProblems: last ? last.error_count + last.warning_count : null,
      })
    : [];
  const passing = checks.filter((c) => c.ok).length;
  const pages = last?.pages ?? null;

  const pack = async () => {
    setPacking(true);
    try {
      setReport(await api.pack(meta.id));
    } catch (e) {
      showToast(messageOf(e, 'Packaging failed.'));
    } finally {
      setPacking(false);
    }
  };

  const freeze = async () => {
    if (!venue) return;
    try {
      const r = await api.freeze(meta.id, venue.id);
      project.value = {
        ...meta,
        submitted_checkpoint: r.checkpoint.commit_sha,
      };
      showToast(`Checkpoint “${r.checkpoint.label}” frozen. Camera-ready diffs use it.`);
    } catch (e) {
      showToast(messageOf(e, 'Could not freeze the submission.'));
    }
  };

  return (
    <>
      <div class="dh">
        <span>Submit</span>
        {venues.length > 0 && (
          <select
            class="sel"
            aria-label="Venue"
            value={venue?.id}
            disabled={!canEdit.value}
            onChange={(e) =>
              void saveSettings({
                venue: (e.target as HTMLSelectElement).value,
              })
            }
          >
            {venues.map((v) => (
              <option key={v.id} value={v.id}>
                {v.name}
              </option>
            ))}
          </select>
        )}
      </div>
      <div class="db">
        {!venue && <div class="empty">Loading venues…</div>}
        {venue && (
          <>
            <div class="meter">
              <b>{pages ?? '–'}</b>
              <span>
                {pages === null
                  ? 'pages · build to count'
                  : `page${pages === 1 ? '' : 's'}${venue.pages ? ` of ${venue.pages}` : ''}`}
              </span>
            </div>
            {venue.pages ? (
              <div class="mbar">
                <i
                  class={pages !== null && pages > venue.pages ? 'over' : ''}
                  style={{
                    width: `${Math.min(100, (100 * (pages ?? 0)) / venue.pages)}%`,
                  }}
                />
              </div>
            ) : (
              <div style={{ height: 8 }} />
            )}
            <div class="hint" style={{ padding: '0 4px 4px' }}>
              {passing} of {checks.length} checks pass
            </div>
            {checks.map((c, i) => (
              <div class="chk-row" key={i}>
                <span class={`ic ${c.na ? 'na' : c.ok ? 'ok' : 'bad'}`}>{c.na ? '–' : c.ok ? '✓' : '✗'}</span>
                <span>
                  {c.text}
                  {c.hint && <div class="sub">{c.hint}</div>}
                </span>
                {c.fix && canEdit.value && (
                  <button
                    class="tb"
                    onClick={() => {
                      const r = applyFix(c.fix!);
                      showToast(r.ok ? 'Applied. Rebuilding.' : r.message);
                      if (r.ok) void runBuild();
                    }}
                  >
                    {c.fix.label}
                  </button>
                )}
              </div>
            ))}
            <div class="acts-row">
              {canCompile.value && (
                <button class="tb primary" disabled={packing} onClick={() => void pack()}>
                  {packing ? 'Packaging…' : `Package for ${venue.short}`}
                </button>
              )}
              {canEdit.value && (
                <button class="tb" onClick={() => void freeze()}>
                  Freeze as submitted
                </button>
              )}
            </div>
            <div class="hint">
              Packaging flattens \input, strips comments, keeps only used figures and bib entries, and proves the result builds
              from scratch before you can download it.
            </div>
            {meta.submitted_checkpoint && (
              <div class="hint">
                Frozen as submitted.{' '}
                {latexdiffAvailable.value ? (
                  <button class="linkish" onClick={() => void comparePdf(meta.submitted_checkpoint!)}>
                    Camera-ready diff
                  </button>
                ) : (
                  'History can compare against it.'
                )}
              </div>
            )}
          </>
        )}
      </div>
      {report && <PackModal venue={venue} report={report} onClose={() => setReport(null)} />}
    </>
  );
}

function PackModal({ venue, report, onClose }: { venue: Venue | undefined; report: PackReport; onClose: () => void }) {
  const id = project.value?.id;
  const errs = report.errors.filter((e) => e.level === 'error');
  return (
    <div class="overlay" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div class="modal" role="dialog" aria-label="Package">
        <div class="mh">
          Package for {venue?.name ?? 'submission'}
          <button class="tb" style={{ marginLeft: 'auto' }} onClick={onClose}>
            Close
          </button>
        </div>
        <div class="mb">
          <div class="sec-h" style={{ marginTop: 0 }}>
            Included
          </div>
          {report.included.map((f) => (
            <div class="file-li" key={f.path}>
              <span class="loc">{f.path}</span>
              <span class="why">{f.why}</span>
            </div>
          ))}
          <div class="sec-h">Left out</div>
          {report.left_out.map((f) => (
            <div class="file-li" key={f.path}>
              <span class="loc">{f.path}</span>
              <span class="why">{f.why}</span>
            </div>
          ))}
          <div class="sec-h">Clean sandbox build</div>
          <div class="chk-row" style={{ border: 0 }}>
            <span class={`ic ${report.build_ok ? 'ok' : 'bad'}`}>{report.build_ok ? '✓' : '✗'}</span>
            <span>
              {report.build_ok
                ? `Compiles from scratch with no cache${report.pages ? ` · ${report.pages} page${report.pages === 1 ? '' : 's'}` : ''}.`
                : `Fails${errs.length ? ` with ${errs.length} error${errs.length === 1 ? '' : 's'}` : ''}. Fix ${errs.length === 1 ? 'it' : 'them'} and package again.`}
              {errs.slice(0, 3).map((e, i) => (
                <div class="sub" key={i}>
                  {e.file ? `${e.file}${e.line ? `:${e.line}` : ''} · ` : ''}
                  {e.message}
                </div>
              ))}
            </span>
          </div>
          <div class="acts-row">
            {report.archive && id ? (
              <a class="tb primary" href={api.packDownloadUrl(id)}>
                Download .tar.gz
              </a>
            ) : (
              <button class="tb primary" disabled title="The package downloads once it builds from scratch">
                Download .tar.gz
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
