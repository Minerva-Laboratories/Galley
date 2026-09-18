import { useEffect, useState } from 'preact/hooks';
import { api, type AgentRun } from '../api';
import { currentSession, project, showToast, suggestions } from '../store/store';
import { ago } from '../util/time';

/** The built-in tasks, in the prototype's order. `args` are the runner arguments a task needs.
 *  `file` marks tasks that take the open file. */
const TASKS: { name: string; title: string; blurb: string; file?: boolean; args?: string }[] = [
  { name: 'fix-build', title: 'Fix build', blurb: 'Reads the error log and patches the cause. You accept the diff.' },
  { name: 'proofread', title: 'Proofread file', blurb: 'Grammar and clarity for the open file, as suggestions, never silent edits.', file: true },
  { name: 'tighten', title: 'Tighten file', blurb: 'Cuts filler and hedging to a target length.', file: true, args: '--arg target=15%' },
  { name: 'cite', title: 'Find a citation', blurb: 'Looks for support in your .bib files; never invents a key.', args: '--arg "claim=…"' },
  { name: 'table', title: 'Table from data', blurb: 'Paste CSV, get a booktabs table with a label.', args: '--arg "data=…"' },
  { name: 'reviewer', title: 'Review as reviewer 2', blurb: 'Comments on weak claims and missing references. No edits.', file: true },
  { name: 'explain', title: 'Explain an error', blurb: "The last build's first error, in plain language." },
];

/** In V1 nothing runs server-side (SPEC §7.5): each task is a `galley agent run` command for a local
 *  model, and the same tasks reach MCP clients as prompts. The token comes from GALLEY_TOKEN so it is
 *  never pasted into a shared screen. */
export function AgentsDrawer() {
  const id = project.value?.id;
  const [runs, setRuns] = useState<AgentRun[]>([]);
  const [now, setNow] = useState(Date.now());
  const suggestionCount = suggestions.value.length;
  useEffect(() => {
    if (!id) return;
    api.agentRuns(id).then(setRuns).catch(() => setRuns([]));
    setNow(Date.now());
  }, [id, suggestionCount]);

  const command = (t: (typeof TASKS)[number]) => {
    const file = currentSession.value?.path ?? 'main.tex';
    const parts = [`galley agent run --url ${api.mcpUrl(id!)} --token $GALLEY_TOKEN --prompt ${t.name}`];
    if (t.file) parts.push(`--arg path=${file}`);
    if (t.args) parts.push(t.args);
    return parts.join(' ');
  };
  const copy = (t: (typeof TASKS)[number]) => {
    const cmd = command(t);
    void navigator.clipboard?.writeText(cmd).then(
      () => showToast('Command copied. Set GALLEY_TOKEN once from Share → AI clients, then paste it in a terminal.'),
      () => showToast(cmd),
    );
  };

  return (
    <>
      <div class="dh">
        <span>Agents</span>
      </div>
      <div class="db">
        <div class="hint" style={{ paddingTop: 0 }}>Agents run on your machine. Nothing leaves it unless you point them at a provider.</div>
        <div class="ag">
          {TASKS.map((t) => (
            <button key={t.name} class="agent" onClick={() => copy(t)} title={command(t)}>
              <div>
                <b>{t.title}</b>
                <span>{t.blurb}</span>
              </div>
            </button>
          ))}
        </div>
        <div class="hint">
          Each task copies a <code>galley agent run</code> command for a local model: Ollama or any OpenAI-compatible
          endpoint, best with a tool-capable model of about 7B or more. Claude Code and other MCP clients get the same
          tasks as prompts. Every result is a suggestion you accept or reject.
        </div>
        <div class="dh" style={{ marginTop: 8 }}>
          <span>Recent runs</span>
        </div>
        {runs.length === 0 && (
          <div class="empty">
            <b>No runs yet</b>
            Runs appear here with who ran them and which model wrote the suggestion.
          </div>
        )}
        {runs.length > 0 && (
          <div class="runs">
            {runs.map((r) => (
              <div class="run" key={r.id}>
                <div class="l">
                  {r.agent} · {r.model ?? 'model not reported'}
                </div>
                <div class="s">
                  <span>{r.author_name}</span>
                  <span>
                    {r.edits} edit{r.edits === 1 ? '' : 's'}
                  </span>
                  <span>{ago(r.created_at, now)}</span>
                </div>
                {r.summary && <div class="sum">{r.summary}</div>}
              </div>
            ))}
          </div>
        )}
      </div>
    </>
  );
}
